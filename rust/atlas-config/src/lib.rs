//! atlas-config — where ATLAS decides what its own settings are.
//!
//! A straight port of `server/config.py`: the SPEC table (every setting with a
//! type, a range and a default), the `ATLAS_*` environment variables,
//! `data/config.json`, the derived paths, the loopback invariant, the file
//! permission hardening, the environment allowlist every child process gets,
//! the model metadata, and the "nothing is downloaded" audit.
//!
//! Three rules shaped the port:
//!
//! * **One implementation per decision.** The loopback verdict is
//!   [`atlas_guard::address::check`] and log redaction is
//!   [`atlas_guard::redact::redact`]; this crate calls them and never
//!   re-implements a URL/IP/redaction rule.
//! * **Nothing is silent.** Python swapped an unreadable value for the default
//!   and said nothing; [`Config::validate`] returns a [`Problem`] that names the
//!   key *and* where the value came from (`env:ATLAS_PORT`,
//!   `data/config.json:app_port`).
//! * **Nothing is downloaded, ever.** [`audit_no_downloads`] is the predicate
//!   that proves it, and it treats its own rules, comments and printed
//!   instructions as documentation rather than as a fetch.
//!
//! Precedence, unchanged: environment > `data/config.json` > default.
//!
//! Nothing here binds a socket except [`free_port`], which asks the kernel about
//! one loopback port at a time and never talks to anything. Nothing is cached in
//! a global except the hardening notes, which are the one thing a caller cannot
//! pass in.
//!
//! # The map
//!
//! * [`spec`] — the SPEC table (every setting with a type, a range, a default),
//!   the `ATLAS_*` names, the host keys, `is_loopback`, and the words a message
//!   about a setting is built from.
//! * [`coerce`] — one number clamped and rounded into its row's range, and
//!   whether the range had to move it.
//! * [`config`] — the effective [`Config`]: precedence, derivation, validation,
//!   `save`.
//! * [`view`] — the JSON views it renders: `values`, `describe`,
//!   `security_report`.
//! * [`ports`] — the three persisted ports: picked once, never re-rolled, never
//!   handed out twice.
//! * [`stored`] — `data/config.json` itself: read, merge-write, 0600.
//! * [`problem`] — where a value came from ([`Origin`]) and what is wrong with it
//!   ([`Problem`], [`ConfigError`]).
//! * [`value`] — a JSON value as the text a setting carries.
//! * [`resolve`] — which path a setting may become (containment, executables).
//! * [`paths`] — path text and the project's directory layout ([`Paths`]).
//! * [`modes`] — 0700 `data/`, 0600 files, and the permission report.
//! * [`spawn`] — the environment allowlist every child process gets.
//! * [`download`] — the rule table: does this line download something?
//! * [`walk`] — the tree walk itself: which files are read, in what order.
//! * [`audit`] — the verdict that proves nothing downloads, with a cache and a
//!   report.
//! * [`warn`] — hardening notes: deduplicated, capped, always redacted.
//! * [`cli`] — the JSON CLI behind the `atlas-config` binary.
//!
//! CLI: one JSON object on stdin, one on stdout —
//! `config | validate | paths | spawn_env | audit`.

#![forbid(unsafe_code)]

mod audit;
mod checks;
mod cli;
mod coerce;
mod config;
mod download;
mod modes;
mod paths;
mod ports;
mod problem;
mod resolve;
mod spawn;
mod spec;
mod stored;
mod value;
mod view;
mod walk;
mod warn;

#[cfg(test)]
mod tests;

pub use audit::{
    audit_cached, audit_no_downloads, downloads_ok, Audit, Finding, AUDIT_CACHE_MAX_AGE,
    AUDIT_EXTENSIONS, AUDIT_GENERATED_DIRS, AUDIT_LIST_MAX, AUDIT_SKIP_DIRS, AUDIT_SKIP_PATHS,
};
pub use checks::{
    import_target, is_settings_request, model_local_path, model_settings, Import, Overrides,
    MODEL_SUFFIX, SETTABLE,
};
pub use coerce::{coerce, coerce_number, Clamp, Coerced};
pub use download::{audit_line_allowed, rule_hit, DownloadRule, DOWNLOAD_RULES};
pub use cli::{main_entry, read_stdin, run_cli, MAX_STDIN, USAGE};
pub use config::{model_path, Config};
pub use ports::{
    free_port, free_port_avoiding, load_managed, load_managed_at, PERSISTED_PORTS, PORT_HIGH,
    PORT_LOW, PORT_WINDOW,
};
pub use problem::{ConfigError, Origin, Problem};
pub use resolve::{resolve_binary, resolve_path, PathPolicy};
pub use stored::{read_stored, write_stored};
pub use modes::{
    acl_inspected, ensure_dir, ensure_file, harden_data_files, is_symlink, permission_basis,
    permission_report, permissions, posix, PermInfo, DATA_MODE, FILE_MODE,
};
pub use paths::{is_executable, Paths};

pub use spawn::{
    allowlist, key_is_secret, platform, snapshot_env, spawn_env, spawn_env_from, valid_extra_name,
    Platform, ENV_ALLOWLIST, ENV_ALLOWLIST_NT,
};
pub use spec::{
    default_model_url, default_text, env_name, host_key_env, host_key_shape, is_host_key,
    is_loopback, key_order, number_text, range_text, requirement_text, spec_of, unusable_kind, Kind,
    Spec, BIND_HOST, ENV, GEO_MAP_URL, GEO_URL, HOST_KEYS, LLAMACPP_RELEASES_API, LLAMACPP_REPO,
    LOOPBACK_HOSTS, MODEL_FILE, MODEL_LOCAL_NAME, MODEL_REPO, SPEC, TEXT_KEYS, WILDCARD_HOSTS,
};
pub use warn::{warn, warnings, WARNING_CAP, WARNING_MAX_CHARS};
