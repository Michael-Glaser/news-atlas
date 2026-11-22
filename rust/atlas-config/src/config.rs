//! The effective configuration: loaded once from file + environment, typed,
//! range-checked, and validated with a name for everything that is wrong.
//!
//! Decisions: the precedence (environment > `data/config.json` > default), the
//! the derived values (ports, model path, browser path), `save`'s merge-and-harden
//! rule, and the two views a caller renders (`describe`, `security_report`).
//!
//! It does not: own the setting vocabulary ([`crate::spec`]), read the
//! environment it is handed ([`crate::spawn::snapshot_env`]), write the file
//! ([`crate::stored`]), or decide what a path may point at ([`crate::resolve`]).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::problem::{ConfigError, Origin, Problem};
use crate::stored::{read_stored, write_stored};

use crate::coerce::{coerce, Clamp};
use crate::paths::{show, Paths};
use crate::resolve::PathPolicy;
use crate::spawn::{key_is_secret, snapshot_env};
use crate::spec::{
    default_text, display_value, env_name, key_order, number_text, number_value, range_text,
    requirement_text, unusable_kind, BIND_HOST, HOST_KEYS, MODEL_FILE, MODEL_REPO, SPEC, TEXT_KEYS,
};
use crate::value::{text_or, value_as_text};
use crate::warn::warn;
/// The effective configuration: typed settings, where each came from, what is
/// wrong with it, and the paths it points at.
///
/// Loaded once and then read by everything else. The numeric fields are typed
/// and range-checked; `sources` and the [`Problem`] list are what makes a
/// failure explain itself.
#[derive(Clone, Debug)]
pub struct Config {
    pub app_port: u16,
    pub llama_port: u16,
    pub cdp_port: u16,
    pub collect_interval_min: f64,
    pub max_age_hours: u32,
    pub window_hours: u32,
    pub retention_days: u32,
    pub model_repo: String,
    pub model_file: String,
    pub model_url: String,
    /// Absolute path pinned to the project (or `""` when it was refused).
    pub model_local: String,
    /// `-c`: the context window handed to llama.cpp.
    pub model_context: u32,
    /// The tokens the model may spend thinking. Not a llama.cpp flag: the agent enforces
    /// it on the request, which is why it is a setting here and not an argv entry.
    pub model_reasoning: u32,
    /// The tokens a final answer may use, enforced at the same place.
    pub model_answer: u32,
    /// `-ngl`; -1 means "llama.cpp decides", 0 means CPU only.
    pub model_gpu_layers: i32,
    /// `-t`; 0 means "llama.cpp decides".
    pub model_threads: u16,
    /// Absolute path to a system browser, or `""`.
    pub browser_path: String,
    /// Unknown keys from `data/config.json`, preserved so a newer file is not
    /// truncated by an older binary.
    pub extra: BTreeMap<String, Value>,
    pub sources: BTreeMap<String, Origin>,
    /// Values that could not be used, with the key and the origin.
    pub bad: Vec<Problem>,
    /// Values ATLAS *did* use, but not as they were written: every number the
    /// SPEC had to clamp, as one sentence naming the place, the value that was
    /// written, the range and the value now in force. A note is not a
    /// [`Problem`] — a clamped value still runs — but it is never a silence
    /// either: `/api/status`'s `describe` shape and `atlas-config config`
    /// publish it under `notes`.
    pub notes: Vec<String>,
    /// The `data/config.json` this was built from, for `validate` and `save`.
    pub stored: Map<String, Value>,
    /// The environment this was built from.
    pub env: BTreeMap<String, String>,
    pub paths: Paths,
}

impl Config {
    /// Read `data/config.json` and the environment, validate, and return the
    /// effective configuration. Pure: it never writes and never re-rolls a port.
    pub fn load_at(paths: &Paths) -> Result<Config, ConfigError> {
        let stored = read_stored(paths)?;
        Ok(Config::build(paths, &snapshot_env(), &stored))
    }

    /// The same, for the process this binary runs in.
    pub fn load() -> Result<Config, ConfigError> {
        Config::load_at(&Paths::discover())
    }

    /// Load and refuse to return anything when a setting is unusable. This is the
    /// boot path: a non-loopback host or a bad port is an error that names the
    /// key and its origin, not a silently repaired value.
    pub fn load_strict_at(paths: &Paths) -> Result<Config, ConfigError> {
        let config = Config::load_at(paths)?;
        let problems = config.validate();
        if problems.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError::Invalid(problems))
        }
    }

    /// `load_strict_at` for the process this binary runs in.
    pub fn load_strict() -> Result<Config, ConfigError> {
        Config::load_strict_at(&Paths::discover())
    }

    /// Build from snapshots — the pure core, and what the tests drive directly.
    ///
    /// Precedence: environment > `data/config.json` > default. A value that is
    /// not a number becomes the default *and* a [`Problem`]; a host key is
    /// dropped in every case, with a note when the value was not loopback.
    pub fn build(paths: &Paths, env: &[(String, String)], stored: &Map<String, Value>) -> Config {
        let env_map: BTreeMap<String, String> = env.iter().cloned().collect();
        let mut merged: Map<String, Value> = Map::new();
        let mut sources: BTreeMap<String, Origin> = BTreeMap::new();

        for spec in SPEC.iter() {
            let key = spec.key;
            let from_env = env_name(key)
                .and_then(|name| env_map.get(name))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            if let Some(value) = from_env {
                let name = env_name(key).unwrap_or_default().to_string();
                merged.insert(key.to_string(), Value::String(value));
                sources.insert(key.to_string(), Origin::Env(name));
            } else if let Some(value) = stored.get(key) {
                merged.insert(key.to_string(), value.clone());
                sources.insert(key.to_string(), Origin::Config);
            } else {
                merged.insert(key.to_string(), spec.default_value());
                sources.insert(key.to_string(), Origin::Default);
            }
        }
        for key in TEXT_KEYS {
            let from_env = env_name(key)
                .and_then(|name| env_map.get(name))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            if let Some(value) = from_env {
                let name = env_name(key).unwrap_or_default().to_string();
                merged.insert(key.to_string(), Value::String(value));
                sources.insert(key.to_string(), Origin::Env(name));
            } else if let Some(value) = stored.get(key) {
                merged.insert(key.to_string(), value.clone());
                sources.insert(key.to_string(), Origin::Config);
            } else {
                merged.insert(key.to_string(), Value::Null);
                sources.insert(key.to_string(), Origin::Default);
            }
        }
        // Unknown keys survive: a file written by a newer binary is forwarded.
        //
        // One family does not: a *credential-shaped* name. Its value is not a
        // setting, it is somebody's secret that was pasted into the wrong file,
        // and this crate republishes what it keeps — `values_json` is what
        // `atlas-config config` prints and what `describe()` carries. `save`
        // already refused to write such a key; `build` kept it, so a file that
        // arrived by hand, by a backup or from a newer binary put the secret on
        // stdout and into every consumer of the values. The name is named and the
        // value is dropped, never echoed.
        for (key, value) in stored.iter() {
            if merged.contains_key(key) {
                continue;
            }
            if key_is_secret(key) {
                warn(&format!(
                    "refusing to use '{}' from data/config.json: its name says it is a credential, and a credential is not a setting",
                    key
                ));
                continue;
            }
            merged.insert(key.clone(), value.clone());
            sources.insert(key.clone(), Origin::Config);
        }

        // Numbers: clamp, then round. A value that is not a number at all is a
        // Problem that names the key — never a silent swap for the default. A
        // value that *is* a number but outside the range is clamped (the Python
        // behaviour) and said out loud in `notes`: the app running on 1024 while
        // the file says 80 is otherwise unexplainable.
        let mut bad: Vec<Problem> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut numbers: BTreeMap<&'static str, f64> = BTreeMap::new();
        for spec in SPEC.iter() {
            let raw = merged.get(spec.key).cloned();
            match coerce(spec, raw.as_ref()) {
                Some(value) => {
                    if value.clamp != Clamp::None {
                        let origin = sources.get(spec.key).cloned().unwrap_or(Origin::Default);
                        notes.push(format!(
                            "{}={} is outside {}; ATLAS is using {}",
                            origin.at(spec.key),
                            display_value(raw.as_ref()).unwrap_or_default(),
                            range_text(spec),
                            number_text(spec, value.value)
                        ));
                    }
                    numbers.insert(spec.key, value.value);
                    merged.insert(spec.key.to_string(), number_value(*spec, value.value));
                }
                None => {
                    numbers.insert(spec.key, spec.default);
                    merged.insert(spec.key.to_string(), spec.default_value());
                    let origin = sources.get(spec.key).cloned().unwrap_or(Origin::Default);
                    if !matches!(origin, Origin::Default | Origin::Auto) {
                        // "is not a usable port" alone leaves a person guessing
                        // what a port is here; the row's own range and the
                        // default now in force are what makes it a fix.
                        bad.push(Problem::new(
                            spec.key,
                            &origin.at(spec.key),
                            display_value(raw.as_ref()),
                            &format!(
                                "{}; ATLAS is using the default {} ({})",
                                unusable_kind(spec),
                                default_text(spec),
                                requirement_text(spec)
                            ),
                        ));
                    }
                }
            }
        }

        // ATLAS has no setting for the bind address; a file or environment that
        // carries one is not trusted to pick one either. See `validate`.
        //
        // Both places are read here: the old loop only looked at `merged`, which
        // never held an environment value for a host key — `ATLAS_HOST=0.0.0.0`
        // was ignored by `build` without a word, and the note that did appear
        // said "from the configuration" even when the value came from a shell.
        //
        // The match is by *shape*, not by exact spelling ([`crate::spec::is_host_key`]):
        // the loop used to call `merged.remove(key)` for the twelve names in the
        // table, so `{"Host": "0.0.0.0"}` stayed in `merged`, was forwarded into
        // `extra` and republished by `values_json`. Nothing in this tree reads a
        // key spelled `Host`, so no bind was smuggled — but that is a property of
        // today's readers, not of this crate, and every future reader of
        // `values_json` would have to know it.
        notes.extend(drop_host_keys(&mut merged, &env_map, &mut sources));

        let model_repo = text_or(merged.get("model_repo"), MODEL_REPO);
        let model_file = text_or(merged.get("model_file"), MODEL_FILE);
        let url_default = crate::default_model_url();
        let model_url = text_or(merged.get("model_url"), &url_default);
        let model_local = crate::resolve_path(
            Some(&value_as_text(merged.get("model_local"))),
            &PathPolicy::model(&paths.root, &show(&paths.model_file)),
        );
        let browser_path = crate::resolve_path(
            Some(&value_as_text(merged.get("browser_path"))),
            &PathPolicy::browser(&paths.root),
        );

        let order = key_order();
        let extra: BTreeMap<String, Value> = merged
            .iter()
            .filter(|(key, _)| !order.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        // The stored file with the values this build normalised, so `save` and
        // `validate` work from the same picture the caller was given.
        let mut stored_updated = stored.clone();
        for spec in SPEC.iter() {
            if stored_updated.contains_key(spec.key) {
                let value = numbers.get(spec.key).copied().unwrap_or(spec.default);
                stored_updated.insert(spec.key.to_string(), number_value(*spec, value));
            }
        }
        // …and without the credential-shaped keys `build` refused to use: this is
        // the map `save` (and anything else holding a `Config`) treats as "the
        // file as ATLAS understands it", and a secret is not part of that.
        let secrets: Vec<String> = stored_updated
            .keys()
            .filter(|key| key_is_secret(key))
            .cloned()
            .collect();
        for key in secrets {
            stored_updated.remove(&key);
        }

        Config {
            app_port: as_u16(numbers.get("app_port").copied().unwrap_or(8787.0)),
            llama_port: as_u16(numbers.get("llama_port").copied().unwrap_or(8080.0)),
            cdp_port: as_u16(numbers.get("cdp_port").copied().unwrap_or(9222.0)),
            collect_interval_min: numbers
                .get("collect_interval_min")
                .copied()
                .unwrap_or(5.0),
            max_age_hours: as_u32(numbers.get("max_age_hours").copied().unwrap_or(72.0)),
            window_hours: as_u32(numbers.get("window_hours").copied().unwrap_or(72.0)),
            retention_days: as_u32(numbers.get("retention_days").copied().unwrap_or(21.0)),
            model_context: as_u32(numbers.get("model_context").copied().unwrap_or(32768.0)),
            model_reasoning: as_u32(numbers.get("model_reasoning").copied().unwrap_or(8192.0)),
            model_answer: as_u32(numbers.get("model_answer").copied().unwrap_or(8192.0)),
            // Signed on purpose: -1 is a value ("llama.cpp decides"), not an absence.
            model_gpu_layers: numbers.get("model_gpu_layers").copied().unwrap_or(-1.0) as i32,
            model_threads: as_u16(numbers.get("model_threads").copied().unwrap_or(0.0)),
            model_repo,
            model_file,
            model_url,
            model_local,
            browser_path,
            extra,
            sources,
            bad,
            notes,
            stored: stored_updated,
            env: env_map,
            paths: paths.clone(),
        }
    }

    /// One numeric setting by name — the table-driven accessor.
    pub fn number(&self, key: &str) -> Option<f64> {
        Some(match key {
            "app_port" => self.app_port as f64,
            "llama_port" => self.llama_port as f64,
            "cdp_port" => self.cdp_port as f64,
            "collect_interval_min" => self.collect_interval_min,
            "max_age_hours" => self.max_age_hours as f64,
            "window_hours" => self.window_hours as f64,
            "retention_days" => self.retention_days as f64,
            "model_context" => self.model_context as f64,
            "model_reasoning" => self.model_reasoning as f64,
            "model_answer" => self.model_answer as f64,
            "model_gpu_layers" => self.model_gpu_layers as f64,
            "model_threads" => self.model_threads as f64,
            _ => return None,
        })
    }

    /// One text setting by name.
    pub fn text(&self, key: &str) -> Option<&str> {
        Some(match key {
            "model_repo" => self.model_repo.as_str(),
            "model_file" => self.model_file.as_str(),
            "model_url" => self.model_url.as_str(),
            "model_local" => self.model_local.as_str(),
            "browser_path" => self.browser_path.as_str(),
            _ => return None,
        })
    }

    pub(crate) fn set_number(&mut self, key: &str, value: f64) {
        match key {
            "app_port" => self.app_port = as_u16(value),
            "llama_port" => self.llama_port = as_u16(value),
            "cdp_port" => self.cdp_port = as_u16(value),
            "collect_interval_min" => self.collect_interval_min = value,
            "max_age_hours" => self.max_age_hours = as_u32(value),
            "window_hours" => self.window_hours = as_u32(value),
            "retention_days" => self.retention_days = as_u32(value),
            "model_context" => self.model_context = as_u32(value),
            "model_reasoning" => self.model_reasoning = as_u32(value),
            "model_answer" => self.model_answer = as_u32(value),
            "model_gpu_layers" => self.model_gpu_layers = value as i32,
            "model_threads" => self.model_threads = as_u16(value),
            _ => {}
        }
    }

    /// Where a setting came from: `env`, `config`, `default` or `auto`.
    pub fn source_of(&self, key: &str) -> String {
        self.sources
            .get(key)
            .map(Origin::source)
            .unwrap_or_else(|| "default".to_string())
    }

    /// Everything wrong with this configuration. Empty means it may run.
    ///
    /// Two questions, the same two Python asked: *may this process listen at
    /// all, and where?* and *is every setting usable?* A non-loopback bind
    /// request — `ATLAS_HOST=0.0.0.0` in the shell, or a hand-edited `"host"`
    /// key in `data/config.json` — is a `Problem`, never a panic and never a
    /// silent fallback to the default.
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems: Vec<Problem> = Vec::new();
        // The same shape match [`Config::build`] drops, so the refusal and the
        // report cannot disagree: a file that spells the key `Host` is *caught*
        // (and named) rather than silently forwarded as an unknown key.
        for known in HOST_KEYS {
            let shape = crate::spec::host_key_shape(known);
            let mut found: Vec<(String, String)> = Vec::new();
            for (key, value) in self.stored.iter() {
                if crate::spec::host_key_shape(key) != shape {
                    continue;
                }
                found.push((
                    format!("data/config.json:{}", key),
                    value_as_text(Some(value)).trim().to_string(),
                ));
            }
            for (variable, value) in self.env.iter() {
                let bare = match variable
                    .strip_prefix("ATLAS_")
                    .or_else(|| variable.strip_prefix("atlas_"))
                {
                    Some(bare) => bare,
                    None => continue,
                };
                if crate::spec::host_key_shape(bare) != shape {
                    continue;
                }
                found.push((format!("env:{}", variable), value.trim().to_string()));
            }
            // The value that will *not* be used is the problem, not the key: a
            // loopback request in a file is tolerated (and dropped), a
            // non-loopback one is reported with its place.
            for (origin, text) in found {
                if text.is_empty() {
                    continue;
                }
                if !crate::is_loopback(&text) {
                    problems.push(Problem::new(
                        known,
                        &origin,
                        Some(text),
                        &format!("is not a loopback address (ATLAS binds {} only)", BIND_HOST),
                    ));
                }
            }
        }
        problems.extend(self.bad.iter().cloned());
        problems
    }

    /// Store overrides in `data/config.json`, validated, and return the effective
    /// configuration.
    ///
    /// Credential-shaped keys are dropped rather than written: ATLAS needs no
    /// secrets, and one handed in by mistake must not reach the disk or
    /// `/api/status`. A host key never survives either — loopback only.
    ///
    /// A number that cannot be used is dropped too (the next load falls back to
    /// the default) and the returned [`Config::notes`] says which key and what
    /// to write instead: a setting that "reverts on its own" after a save is a
    /// sentence, not a mystery.
    pub fn save(paths: &Paths, overrides: &Map<String, Value>) -> Result<Config, ConfigError> {
        let mut stored = read_stored(paths)?;
        for (key, value) in overrides {
            stored.insert(key.clone(), value.clone());
        }
        let keys: Vec<String> = stored.keys().cloned().collect();
        for key in keys {
            if key_is_secret(&key) {
                stored.remove(&key);
                warn(&format!("refusing to store '{}' in data/config.json", key));
            }
        }
        let mut dropped: Vec<String> = Vec::new();
        for spec in SPEC.iter() {
            if let Some(value) = stored.get(spec.key).cloned() {
                match coerce(spec, Some(&value)) {
                    Some(number) => {
                        stored.insert(spec.key.to_string(), number_value(*spec, number.value));
                    }
                    None => {
                        dropped.push(format!(
                            "data/config.json:{}={} was not stored: {} ({})",
                            spec.key,
                            value_as_text(Some(&value)),
                            unusable_kind(spec),
                            requirement_text(spec)
                        ));
                        stored.remove(spec.key);
                    }
                }
            }
        }
        for (key, policy) in [
            ("model_local", PathPolicy::model(&paths.root, &show(&paths.model_file))),
            ("browser_path", PathPolicy::browser(&paths.root)),
        ] {
            if let Some(value) = stored.get(key).cloned() {
                let resolved = crate::resolve_path(Some(&value_as_text(Some(&value))), &policy);
                stored.insert(key.to_string(), Value::String(resolved));
            }
        }
        for key in HOST_KEYS {
            stored.remove(key); // loopback only: never persisted
        }
        // …in every spelling, not just the twelve the table holds: a save is the
        // one place this crate writes the file, so it must not write back a key
        // it refuses to honour.
        let spellings: Vec<String> = stored
            .keys()
            .filter(|key| crate::spec::is_host_key(key))
            .cloned()
            .collect();
        for key in spellings {
            stored.remove(&key);
            warn(&format!(
                "refusing to store '{}' in data/config.json: ATLAS has no bind-address setting (it binds {} only)",
                key, BIND_HOST
            ));
        }
        write_stored(paths, &stored)?;
        let mut config = Config::load_at(paths)?;
        // The reload cannot know what was dropped on the way in; the caller asked
        // for these values, so it is told.
        config.notes.splice(0..0, dropped);
        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// the loopback invariant, applied
// ---------------------------------------------------------------------------

/// Remove every key that asks for a bind address, in every spelling, and answer
/// one note per value found — the loopback-invariant half of [`Config::build`].
///
/// A host key is not a setting (`spec::HOST_KEYS`): it is dropped whether it came
/// from the file or the environment, and the note says which of the two, because
/// "ignoring '0.0.0.0' from the configuration" once sent a person to a file that
/// did not carry the value. `ATLAS_*` is stripped before the shape is compared,
/// so `ATLAS_BIND_HOST` and a `"bind-host"` key in the file are the same request
/// as `ATLAS_HOST`/`"host"`.
fn drop_host_keys(
    merged: &mut Map<String, Value>,
    env_map: &BTreeMap<String, String>,
    sources: &mut BTreeMap<String, Origin>,
) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();
    for known in HOST_KEYS {
        let shape = crate::spec::host_key_shape(known);
        // The environment first, then the file: the order a person reading the
        // notes expects (the shell wins over the file elsewhere in this crate).
        for (variable, value) in env_map.iter() {
            let bare = match variable
                .strip_prefix("ATLAS_")
                .or_else(|| variable.strip_prefix("atlas_"))
            {
                Some(bare) => bare,
                // Only the `ATLAS_` family is ours: a bare `HOST` in the shell is
                // somebody else's variable and noting it would be noise.
                None => continue,
            };
            if crate::spec::host_key_shape(bare) != shape {
                continue;
            }
            let text = value.trim().to_string();
            if !text.is_empty() {
                notes.push(host_note(&format!("env:{}", variable), &text));
            }
        }
        // Every spelling in the file, not just the one the table holds.
        let spellings: Vec<String> = merged
            .keys()
            .filter(|key| crate::spec::host_key_shape(key) == shape)
            .cloned()
            .collect();
        for key in spellings {
            let value = merged.remove(&key);
            sources.remove(&key);
            let text = value
                .map(|value| value_as_text(Some(&value)).trim().to_string())
                .unwrap_or_default();
            if !text.is_empty() {
                notes.push(host_note(&format!("data/config.json:{}", key), &text));
            }
        }
    }
    notes
}

/// The sentence for a dropped host key: one for a value that was loopback (still
/// not a setting) and one for a value that was not, which is the one worth
/// reading twice.
fn host_note(place: &str, text: &str) -> String {
    if crate::is_loopback(text) {
        format!(
            "{}='{}' was ignored: ATLAS has no bind-address setting (it binds {} only)",
            place, text, BIND_HOST
        )
    } else {
        format!(
            "{}='{}' is not loopback and was ignored: ATLAS binds {} only",
            place, text, BIND_HOST
        )
    }
}

fn as_u16(value: f64) -> u16 {
    if value.is_finite() && value >= 0.0 && value <= u16::MAX as f64 {
        value as u16
    } else {
        0
    }
}

fn as_u32(value: f64) -> u32 {
    if value.is_finite() && value >= 0.0 && value <= u32::MAX as f64 {
        value as u32
    } else {
        0
    }
}


// ---------------------------------------------------------------------------
// the model file
// ---------------------------------------------------------------------------

/// The GGUF file ATLAS will load: the configured one, else the newest in
/// `models/` (`sorted(...)[-1]`, exactly as the Python did).
pub fn model_path(config: &Config, paths: &Paths) -> String {
    let configured = config.model_local.trim();
    if !configured.is_empty() && crate::paths::path_is_file(configured) {
        return configured.to_string();
    }
    let entries = match std::fs::read_dir(&paths.models_dir) {
        Ok(entries) => entries,
        Err(_) => return String::new(),
    };
    let mut found: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_ascii_lowercase().ends_with(".gguf"))
                    .unwrap_or(false)
        })
        .map(|path| show(&path))
        .collect();
    found.sort();
    found.pop().unwrap_or_default()
}
