//! The two JSON views a caller renders: `values`/`describe` (what the settings
//! became, where each came from, the files they point at) and `security_report`
//! (the machine-hardening snapshot `/api/security` publishes).
//!
//! They live in their own file because they are the *answer* side of this crate:
//! [`crate::Config::build`] decides, these render. Every key here is one
//! `config.py` published, plus `notes` — the values ATLAS had to move — so a
//! caller can explain a setting instead of only reporting it.
//!
//! It does not: read the file, validate anything ([`crate::Config::validate`]), or
//! decide what a path may point at ([`crate::resolve`]). Nothing here writes.

use serde_json::{Map, Value};

use crate::modes::{DATA_MODE, FILE_MODE};
use crate::paths::{file_info, mode_text, show};
use crate::problem::Problem;
use crate::spec::{key_order, number_value, spec_of, BIND_HOST};
use crate::warn::warnings;
use crate::Config;

impl Config {
    /// `values` as JSON: every setting, the extras, and nothing invented.
    ///
    /// Every **string** value is put through `atlas_guard`'s redaction before it is inserted,
    /// so this object is safe to print. Two reasons, and they are different holes:
    ///
    /// * a *key* whose name is a credential is dropped entirely (below) — that is the file's
    ///   shape being wrong;
    /// * a *value* can carry a credential while the key is an ordinary setting. The measured
    ///   case is `model_url = "http://alice:secret@127.0.0.1:8080"`: the name says nothing,
    ///   the value is somebody's password, and this object is what `atlas-config config`
    ///   prints (`cli.rs:91`) and what `describe()` carries. It printed verbatim until a test
    ///   in `tests/config_file_modes.rs` measured it.
    ///
    /// The replacement keeps the shape (`http://[redacted]@127.0.0.1:8080`), so a person
    /// reading the output can still tell which setting is wrong and go and fix it — a whole
    /// value replaced by a marker would hide the address as well as the password.
    pub fn values_json(&self) -> Value {
        let mut map = Map::new();
        for key in key_order() {
            match spec_of(key) {
                Some(spec) => {
                    let value = self.number(key).unwrap_or(spec.default);
                    map.insert(key.to_string(), number_value(*spec, value));
                }
                None => {
                    let text = self.text(key).unwrap_or("");
                    map.insert(
                        key.to_string(),
                        Value::String(atlas_guard::redact::redact(text).text),
                    );
                }
            }
        }
        for (key, value) in &self.extra {
            // A credential is not a setting, and this object is printed. `build`
            // already refuses to keep such a key (and says so in `security.warnings`);
            // this is the second door, because `Config`'s fields are public and a
            // caller can build one with anything in `extra`.
            if crate::key_is_secret(key) {
                continue;
            }
            map.insert(key.clone(), redacted(value));
        }
        Value::Object(map)
    }

    /// Effective config with provenance and file checks — the `describe()` shape
    /// `/api/status` and the ops check read.
    ///
    /// Not ported here: the `network` block (the listener audit, `lsof`/`netstat`
    /// + the child registry). That is process supervision, not configuration,
    /// and it belongs to whoever owns the listener.
    pub fn describe(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "config_file".to_string(),
            Value::String(show(&self.paths.config_file)),
        );
        root.insert("root".to_string(), Value::String(show(&self.paths.root)));
        root.insert("values".to_string(), self.values_json());
        let mut sources = Map::new();
        for key in key_order() {
            sources.insert(key.to_string(), Value::String(self.source_of(key)));
        }
        root.insert("sources".to_string(), Value::Object(sources));
        let mut files = Map::new();
        files.insert("model".to_string(), file_info(&self.model_local));
        files.insert(
            "model_resolved".to_string(),
            file_info(&crate::config::model_path(self, &self.paths)),
        );
        files.insert("browser".to_string(), file_info(&self.browser_path));
        files.insert("geo".to_string(), file_info(&show(&self.paths.geo_local)));
        files.insert(
            "geo_map".to_string(),
            file_info(&show(&self.paths.geo_map_local)),
        );
        files.insert(
            "llama_server".to_string(),
            file_info(&show(&self.paths.llama_local)),
        );
        root.insert("files".to_string(), Value::Object(files));
        let problems: Vec<Value> = self.validate().iter().map(Problem::to_json).collect();
        root.insert("problems".to_string(), Value::Array(problems));
        // The values ATLAS moved (a clamped port) next to the ones it refused:
        // "it runs on 1024 but the file says 80" is the one question this shape
        // could not answer before.
        root.insert(
            "notes".to_string(),
            Value::Array(
                self.notes
                    .iter()
                    .map(|note| Value::String(note.clone()))
                    .collect(),
            ),
        );
        root.insert("security".to_string(), self.security_report());
        Value::Object(root)
    }

    /// Machine-hardening snapshot for `/api/status` and `/api/security`. Counts
    /// and reasons only — never a page, never a prompt.
    ///
    /// `loopback_only` is a constant claim: ATLAS binds [`BIND_HOST`] whatever a
    /// file or an environment variable asks for. The *request* that asked for
    /// something else is therefore reported too ([`Config::validate`]'s
    /// problems): without them a person reading this block saw "loopback only:
    /// yes" and no hint that `ATLAS_HOST=0.0.0.0` was ignored.
    ///
    /// Everything about the file system in here is a measurement, and the four
    /// keys that say what was measured are the difference between a report and a
    /// wish:
    ///
    /// * `data_mode`/`file_mode` — the mode `data/` and `data/config.json` have
    ///   *now* (`""` when they do not exist). `target_modes` carries the numbers
    ///   ATLAS wants, labelled as such.
    /// * `permissions.ok` — false unless something was read and everything read
    ///   is owner-only, so a report about a directory that does not exist yet is
    ///   not a clean bill of health.
    /// * `permissions.data_inside_project` — false when `<root>/data` resolves
    ///   outside the project root (a link), which is the archive being redirected
    ///   somewhere the layout does not name.
    /// * `permissions.basis`/`acl_inspected` — `posix-mode` where a mode was read;
    ///   `windows-acl-inherited` where privacy is an ACL this crate does not
    ///   inspect, so `ok` there is "nothing was found exposed", not "a private ACL
    ///   was seen".
    pub fn security_report(&self) -> Value {
        let mut map = Map::new();
        map.insert("bind_host".to_string(), Value::String(BIND_HOST.to_string()));
        map.insert("loopback_only".to_string(), Value::Bool(true));
        let problems: Vec<Value> = self.validate().iter().map(Problem::to_json).collect();
        map.insert("problems".to_string(), Value::Array(problems));
        let mut ports = Map::new();
        ports.insert("http".to_string(), Value::from(self.app_port));
        ports.insert("llama".to_string(), Value::from(self.llama_port));
        ports.insert("cdp".to_string(), Value::from(self.cdp_port));
        map.insert("ports".to_string(), Value::Object(ports));
        map.insert("shell".to_string(), Value::Bool(false));
        map.insert(
            "env_allowlist".to_string(),
            Value::Array(
                crate::allowlist(crate::platform())
                    .iter()
                    .map(|name| Value::String((*name).to_string()))
                    .collect(),
            ),
        );
        // Every file ATLAS writes into `data/` — the logs included, because
        // `harden_data_files` chmods them and a report that checked only the
        // config and the database said "ok" about files it never looked at.
        let paths: Vec<String> = vec![
            show(&self.paths.data_dir),
            show(&self.paths.config_file),
            show(&self.paths.db),
            show(&self.paths.db_wal),
            show(&self.paths.db_shm),
            show(&self.paths.log_app),
            show(&self.paths.log_llama),
            show(&self.paths.log_chrome),
        ];
        let (ok, entries) = crate::permission_report(&paths);
        // `data_mode`/`file_mode` are what the file system *answered*, not what
        // ATLAS wanted: the report used to publish the two constants, so a `data/`
        // at 0777 was reported next to `"data_mode": "0o700"` — a mode nobody
        // read. The intended numbers are still published, under `target_modes`,
        // where they cannot be mistaken for a measurement.
        let dir_now = crate::permissions(&show(&self.paths.data_dir));
        let file_now = crate::permissions(&show(&self.paths.config_file));
        map.insert("data_mode".to_string(), Value::String(dir_now.mode.clone()));
        map.insert("file_mode".to_string(), Value::String(file_now.mode.clone()));
        map.insert(
            "target_modes".to_string(),
            serde_json::json!({
                "data": mode_text(DATA_MODE),
                "file": mode_text(FILE_MODE),
            }),
        );
        // The archive is `<root>/data`, and `ATLAS_DATA_DIR` is the only thing
        // that moves it. A `data/` that is a symlink to a directory elsewhere is
        // therefore a redirect nobody asked for: the app would write the archive,
        // the settings and the logs outside the project while every path it
        // publishes still reads `<root>/data`. Symlinks are resolved here so the
        // report says so, and `ok` cannot be true while it is happening.
        let data_inside = crate::paths::inside_real(
            &show(&self.paths.root),
            &show(&self.paths.data_dir),
        );
        let mut report = Map::new();
        report.insert("posix".to_string(), Value::Bool(crate::modes::posix()));
        report.insert(
            "basis".to_string(),
            Value::String(crate::modes::permission_basis(crate::platform()).to_string()),
        );
        report.insert(
            "acl_inspected".to_string(),
            Value::Bool(crate::modes::acl_inspected(crate::platform())),
        );
        report.insert("data_inside_project".to_string(), Value::Bool(data_inside));
        report.insert(
            "entries".to_string(),
            Value::Array(entries.iter().map(|entry| entry.to_json()).collect()),
        );
        report.insert("ok".to_string(), Value::Bool(ok && data_inside));
        map.insert("permissions".to_string(), Value::Object(report));
        map.insert(
            "warnings".to_string(),
            Value::Array(warnings().iter().map(|note| Value::String(note.clone())).collect()),
        );
        Value::Object(map)
    }
}

/// The redacted form of a stored value: a string through the guard's redaction, an array or
/// object member by member, anything else unchanged.
///
/// `values_json` prints what ATLAS is using, and an unknown key's value can be any JSON — a
/// list of URLs, an object of endpoints. Redacting only the top-level strings would leave the
/// nested ones, which is where a real settings file keeps them.
fn redacted(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(atlas_guard::redact::redact(text).text),
        Value::Array(items) => Value::Array(items.iter().map(redacted).collect()),
        Value::Object(fields) => {
            let mut out = Map::new();
            for (key, item) in fields {
                out.insert(key.clone(), redacted(item));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_secret_in_an_unknown_key_is_redacted_too() {
        // A settings file keeps its endpoints inside an object, so redacting only the
        // top-level strings would miss the one that matters. Every string at any depth.
        let mut map = Map::new();
        map.insert(
            "mirrors".to_string(),
            serde_json::json!({
                "primary": {"url": "https://alice:secret@mirror.example/feed", "port": 8080},
                "list": ["https://token.example/?q=x&t=Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq"],
            }),
        );
        let redacted = redacted(&Value::Object(map));
        let text = redacted.to_string();
        assert!(!text.contains("alice:secret@"), "{text}");
        assert!(!text.contains("Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq"), "{text}");
        // Numbers and booleans are settings, not text: they pass through untouched.
        assert!(text.contains("\"port\":8080"), "{text}");
        assert!(redacted.is_object());
    }

    #[test]
    fn a_citation_and_a_headline_survive_the_redaction() {
        // The other half of the rule: the printed values must still be readable, or nobody
        // can tell which setting is wrong.
        for text in [
            "https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/",
            "doi:10.1038/s41586-021-03819-2",
            "Nvidia rose 3 percent on Tuesday after TSMC reported demand.",
        ] {
            assert_eq!(redacted(&Value::String(text.to_string())), Value::String(text.to_string()));
        }
    }
}
