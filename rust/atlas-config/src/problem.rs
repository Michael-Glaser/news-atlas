//! Where a value came from, and what is wrong with it — the two things a caller
//! needs to fix a setting.
//!
//! Decisions: the label for each provenance (`env:ATLAS_PORT`, `data/config.json`,
//! `auto`, `default`), and that a bad setting becomes a [`Problem`] naming the key
//! *and* the place, never a silently repaired value (`config.py` swapped in the
//! default and said nothing).
//!
//! It does not: read the file, read the environment, or decide which values are
//! legal — [`crate::Config::validate`] collects the problems and this module
//! formats them.

use std::fmt;

use serde_json::{Map, Value};

/// Where a setting came from. `Auto` is a port picked on first start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Env(String),
    Config,
    Default,
    Auto,
}

impl Origin {
    /// `env` / `config` / `default` / `auto` — the word `describe` reports.
    pub fn source(&self) -> String {
        match self {
            Origin::Env(_) => "env".to_string(),
            Origin::Config => "config".to_string(),
            Origin::Default => "default".to_string(),
            Origin::Auto => "auto".to_string(),
        }
    }

    /// The place a message names: `env:ATLAS_PORT`, `data/config.json`, …
    pub fn place(&self) -> String {
        match self {
            Origin::Env(name) => format!("env:{}", name),
            Origin::Config => "data/config.json".to_string(),
            Origin::Default => "default".to_string(),
            Origin::Auto => "auto".to_string(),
        }
    }

    /// True when the value came from the environment.
    pub fn from_env(&self) -> bool {
        matches!(self, Origin::Env(_))
    }

    /// `env:ATLAS_PORT` or `data/config.json:app_port`.
    pub(crate) fn at(&self, key: &str) -> String {
        match self {
            Origin::Env(name) => format!("env:{}", name),
            Origin::Config => format!("data/config.json:{}", key),
            Origin::Default => "default".to_string(),
            Origin::Auto => "auto".to_string(),
        }
    }
}


/// One thing that is wrong, with the setting it belongs to and where the value
/// came from — so a boot failure says `env:ATLAS_PORT='banana'` and not "bad
/// config".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub key: String,
    /// `env:ATLAS_PORT` or `data/config.json:app_port`.
    pub origin: String,
    /// The offending value, as it will be shown.
    pub value: Option<String>,
    pub detail: String,
}

impl Problem {
    pub(crate) fn new(key: &str, origin: &str, value: Option<String>, detail: &str) -> Problem {
        Problem {
            key: key.to_string(),
            origin: origin.to_string(),
            value,
            detail: detail.to_string(),
        }
    }

    /// The whole sentence, e.g.
    /// `data/config.json:host='0.0.0.0' is not a loopback address (ATLAS binds 127.0.0.1 only)`.
    pub fn message(&self) -> String {
        match &self.value {
            Some(value) => format!("{}='{}' {}", self.origin, value, self.detail),
            None => format!("{} {}", self.origin, self.detail),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("key".to_string(), Value::String(self.key.clone()));
        map.insert("origin".to_string(), Value::String(self.origin.clone()));
        map.insert(
            "value".to_string(),
            match &self.value {
                Some(value) => Value::String(value.clone()),
                None => Value::Null,
            },
        );
        map.insert("detail".to_string(), Value::String(self.detail.clone()));
        map.insert("message".to_string(), Value::String(self.message()));
        Value::Object(map)
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// Why configuration could not be used. Either the store itself is unusable
/// (unreadable, not JSON, not an object) or one or more settings are — each
/// naming its own key and origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// `data/config.json` could not be read at all.
    File { origin: String, detail: String },
    /// Values that cannot be used. Never a panic: a hostile environment is a
    /// `Problem`, not a crash.
    Invalid(Vec<Problem>),
}

impl ConfigError {
    /// The first offending key, when the failure belongs to one.
    pub fn key(&self) -> Option<&str> {
        match self {
            ConfigError::File { .. } => None,
            ConfigError::Invalid(problems) => problems.first().map(|problem| problem.key.as_str()),
        }
    }

    /// Where the offending value came from (`env:ATLAS_X`, `data/config.json:x`).
    pub fn origin(&self) -> Option<String> {
        match self {
            ConfigError::File { origin, .. } => Some(origin.clone()),
            ConfigError::Invalid(problems) => problems.first().map(|problem| problem.origin.clone()),
        }
    }

    /// Every problem behind this error.
    pub fn problems(&self) -> &[Problem] {
        match self {
            ConfigError::File { .. } => &[],
            ConfigError::Invalid(problems) => problems,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::File { origin, detail } => write!(f, "{}: {}", origin, detail),
            ConfigError::Invalid(problems) => {
                let mut first = true;
                for problem in problems {
                    if !first {
                        f.write_str("; ")?;
                    }
                    first = false;
                    f.write_str(&problem.message())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::config::Config;


#[test]
fn the_error_type_names_the_key_and_the_place() {
    let dir = TempDir::new("error-keys");
    let env = env_pairs(&[("ATLAS_PORT", "banana")]);
    let config = Config::build(&dir.at(), &env, &empty());
    let error = ConfigError::Invalid(config.validate());
    assert_eq!(error.key(), Some("app_port"));
    assert_eq!(error.origin().as_deref(), Some("env:ATLAS_PORT"));
    assert_eq!(error.problems().len(), 1);
    // This assertion used to pin `"env:ATLAS_PORT='banana' is not a usable
    // port"` and nothing else. That sentence says what is wrong and stops: the
    // range and the value now in force were left to a person's guess. The
    // sentence is pinned here in its full form instead — the key, the place, the
    // reason, the default that took over and what to write — because the boot
    // refusal is the one message a stuck user reads.
    assert_eq!(
        error.to_string(),
        "env:ATLAS_PORT='banana' is not a usable port; ATLAS is using the default 8787 \
         (write a whole number from 1024 to 65535)"
    );

    let dir = TempDir::new("error-file");
    dir.write("data/config.json", "{oops");
    let error = Config::load_strict_at(&dir.at()).expect_err("bad file");
    assert_eq!(error.key(), None);
    assert!(error.origin().unwrap().starts_with("data/config.json"));
    assert!(error.to_string().contains("is not valid JSON"));
}

#[test]
fn origin_labels_are_the_documented_strings() {
    assert_eq!(Origin::Env("ATLAS_PORT".into()).place(), "env:ATLAS_PORT");
    assert_eq!(Origin::Config.place(), "data/config.json");
    assert_eq!(Origin::Auto.source(), "auto");
    assert_eq!(Origin::Default.source(), "default");
    assert_eq!(Origin::Config.source(), "config");
    assert!(Origin::Env("X".into()).from_env());
    assert!(!Origin::Config.from_env());
}
}
