//! `wire` — what the `atlas-monitor` binary answers, one op at a time.
//!
//! Decisions: which ops exist ([`OPS`]), what each one answers, that a missing
//! required field is an error rather than a default, and that a reason a caller
//! sent is validated against [`Reason::ALL`] instead of stored as typed.
//!
//! It does not: decide anything a check decides. Every op here is a thin call into
//! [`crate::service::MonitorService`], which is what the server and the agent tools
//! use too — so the bridge and the app cannot get different answers.
//!
//! Two ops are slow on purpose: `run` and `run_all` drive one research pass each,
//! and the model decides how long that takes. `run_all` and `tick` with
//! `"run": true` are what the button and the scheduler call.
//!
//! ```text
//! atlas-monitor run   <<< '{"id": 3, "reason": "button"}'
//! atlas-monitor tick  <<< '{"run": false}'
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use atlas_config::Paths;
use atlas_store::Store;

use crate::error::Error;
use crate::service::MonitorService;
use crate::spec::{Draft, Reason};
use crate::schedule::{Scheduler, SystemClock};

/// Every op this binary answers.
pub const OPS: [&str; 14] = [
    "init",
    "list",
    "add",
    "set_active",
    "set_cadence",
    "delete",
    "run",
    "run_all",
    "tick",
    "alerts",
    "mark_seen",
    "unseen",
    "runs",
    "status",
];

/// The default number of alerts one `alerts` call returns (`monitor.py` used 30).
pub const ALERT_LIMIT: i64 = 30;

/// The default lookback of `alerts` (`monitor.py::recent_alerts` used 24 hours).
pub const ALERT_HOURS: i64 = 24;

/// One question, one answer. Called by `main.rs` through `atlas_lib::cli`.
pub fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(raw).map_err(|_| "input must be a JSON object".to_string())?
    };
    if !input.is_object() {
        return Err("input must be a JSON object".to_string());
    }
    if !OPS.contains(&cmd) {
        return Err(format!(
            "unknown subcommand: {} (expected one of: {})",
            cmd,
            OPS.join(", ")
        ));
    }
    let paths = root_paths(&input);
    let store = Arc::new(
        Store::open(db_path(&input, &paths)).map_err(|error| error.message().to_string())?,
    );
    answer(cmd, &input, &paths, store).map_err(|error| error.message().to_string())
}

fn answer(
    cmd: &str,
    input: &Value,
    paths: &Paths,
    store: Arc<Store>,
) -> Result<String, Error> {
    match cmd {
        "init" => {
            // The base schema too: a monitor check reads `articles`, and asking for
            // the monitor tables alone on an empty file would leave a database that
            // cannot answer. Both calls are idempotent.
            let base = store.init()?;
            let applied = store.init_monitors()?;
            Ok(json!({
                "ok": true,
                "migrations_applied": applied.migrations_applied,
                "migrations_skipped": applied.migrations_skipped,
                "indexes": applied.indexes,
                "unique_url_index": applied.unique_url_index,
                "base_migrations": base.migrations_applied,
                "db": store.path().display().to_string(),
            })
            .to_string())
        }
        "list" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let monitors: Vec<Value> = service.list()?.iter().map(crate::spec::Monitor::to_json).collect();
            Ok(json!({ "monitors": monitors, "count": monitors.len() }).to_string())
        }
        "add" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let draft = Draft::from_json(input)?;
            let id = service.add(&draft)?;
            Ok(json!({
                "ok": true,
                "id": id,
                "threshold": draft.threshold,
                "cadence_minutes": draft.cadence_minutes,
            })
            .to_string())
        }
        "set_active" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let id = need_int(input, "id")?;
            // Required, like every other field this module calls required: a
            // caller that forgot `active` used to switch the monitor *on*, which
            // is the one guess this binary must not make about a user's setting.
            let active = need_bool(input, "active")?;
            Ok(json!({ "ok": true, "changed": service.set_active(id, active)? }).to_string())
        }
        "set_cadence" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let id = need_int(input, "id")?;
            let minutes = need_int(input, "cadence_minutes")?;
            Ok(json!({ "ok": true, "changed": service.set_cadence(id, minutes)? }).to_string())
        }
        "delete" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let id = need_int(input, "id")?;
            Ok(json!({ "ok": true, "changed": service.delete(id)? }).to_string())
        }
        "alerts" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let hours = input
                .get("hours")
                .and_then(Value::as_i64)
                .unwrap_or(ALERT_HOURS);
            let limit = input
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(ALERT_LIMIT);
            let monitor = input.get("monitor").and_then(Value::as_str);
            let alerts = service.alerts(hours, limit, monitor)?;
            Ok(json!({
                "alerts": alerts,
                "count": alerts.len(),
                "unseen": service.unseen()?,
            })
            .to_string())
        }
        "mark_seen" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let id = need_int(input, "id")?;
            let seen = input.get("seen").and_then(Value::as_bool).unwrap_or(true);
            Ok(json!({ "ok": true, "changed": service.mark_seen(id, seen)? }).to_string())
        }
        "unseen" => {
            let service = MonitorService::reader(Arc::clone(&store));
            Ok(json!({ "unseen": service.unseen()? }).to_string())
        }
        "runs" => {
            let service = MonitorService::reader(Arc::clone(&store));
            let id = input.get("id").and_then(Value::as_i64);
            let limit = input.get("limit").and_then(Value::as_i64).unwrap_or(20);
            let runs = service.runs(id, limit)?;
            Ok(json!({ "runs": runs, "count": runs.len() }).to_string())
        }
        "status" => {
            let service = scheduler_service(input, paths, Arc::clone(&store))?;
            Ok(service.status().to_string())
        }
        "run" => {
            let service = native_service(input, paths, Arc::clone(&store))?;
            let id = need_int(input, "id")?;
            let reason = reason_of(input)?;
            Ok(service.run(id, reason).to_json().to_string())
        }
        "run_all" => {
            let service = native_service(input, paths, Arc::clone(&store))?;
            let reason = reason_of(input)?;
            let reports = service.run_all(reason);
            let new_alerts: usize = reports.iter().map(|report| report.new_alerts()).sum();
            Ok(json!({
                "ok": true,
                "checked": reports.iter().filter(|report| report.ok()).count(),
                "new_alerts": new_alerts,
                "monitors": reports.iter().map(|report| report.to_json()).collect::<Vec<_>>(),
            })
            .to_string())
        }
        "tick" => {
            let run_due = input.get("run").and_then(Value::as_bool).unwrap_or(false);
            let service = if run_due {
                native_service(input, paths, Arc::clone(&store))?
            } else {
                scheduler_service(input, paths, Arc::clone(&store))?
            };
            Ok(service.tick(run_due)?.to_json().to_string())
        }
        other => Err(Error::new(format!("unknown subcommand: {}", other))),
    }
}

/// A service that can read and schedule but runs no research (a list, an alert
/// page, a status line, a "who is due" tick).
fn scheduler_service(input: &Value, _paths: &Paths, store: Arc<Store>) -> Result<MonitorService, Error> {
    let service = MonitorService::reader(store);
    Ok(apply_scheduler(input, service))
}

/// A service wired for real: `atlas-ingest`'s pipeline for the research pass and
/// the local model for the two questions.
fn native_service(input: &Value, paths: &Paths, store: Arc<Store>) -> Result<MonitorService, Error> {
    // A check reads `articles`, so the base schema has to exist before the research
    // pipeline opens the archive. Idempotent, and the same call `atlas-ingest`'s own
    // binary makes.
    store.init()?;
    let service = MonitorService::native(store, &paths.root, Arc::new(SystemClock::new()))?;
    Ok(apply_scheduler(input, service))
}

/// The scheduler's on/off state, from `"scheduler"` or a `"enabled"` field.
///
/// Nothing in this binary keeps state between calls, so an explicit field is how a
/// caller says "the user switched the scheduler off".
fn apply_scheduler(input: &Value, service: MonitorService) -> MonitorService {
    let enabled = input
        .get("scheduler")
        .and_then(Value::as_bool)
        .or_else(|| input.get("enabled").and_then(Value::as_bool))
        .unwrap_or(true);
    let mut scheduler = Scheduler::new(enabled);
    scheduler.set_enabled(enabled);
    service.with_scheduler(scheduler)
}

/// The project the caller means: `"root"` when given, else the discovered one.
fn root_paths(input: &Value) -> Paths {
    match input.get("root").and_then(Value::as_str) {
        Some(root) if !root.trim().is_empty() => Paths::at(root),
        _ => Paths::discover(),
    }
}

/// The database: `"db"` when given, else the one this project owns.
fn db_path(input: &Value, paths: &Paths) -> PathBuf {
    match input.get("db").and_then(Value::as_str) {
        Some(db) if !db.trim().is_empty() => PathBuf::from(db),
        _ => paths.db.clone(),
    }
}

/// A field that must be there and must be a boolean. `1`/`0` and "on"/"off" are
/// refused rather than guessed: a setting the user owns is not a place to be
/// clever.
fn need_bool(input: &Value, key: &str) -> Result<bool, Error> {
    match input.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Error::new(format!(
            "\"{}\" must be true or false",
            key
        ))),
        None => Err(Error::new(format!("missing \"{}\"", key))),
    }
}

fn need_int(input: &Value, key: &str) -> Result<i64, Error> {
    match input.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| Error::new(format!("\"{}\" must be a whole number", key))),
        Some(Value::String(text)) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| Error::new(format!("\"{}\" must be a whole number", key))),
        _ => Err(Error::new(format!("missing \"{}\"", key))),
    }
}

/// The reason a check ran. A misspelling is refused: the log has to be readable.
fn reason_of(input: &Value) -> Result<Reason, Error> {
    match input.get("reason").and_then(Value::as_str) {
        None => Ok(Reason::Button),
        Some(raw) => Reason::parse(raw).ok_or_else(|| {
            Error::new(format!(
                "unknown reason \"{}\" (expected one of: {})",
                crate::error::scrub(raw),
                Reason::ALL
                    .iter()
                    .map(|reason| reason.as_str())
                    .collect::<Vec<&str>>()
                    .join(", ")
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_is_listed_once() {
        let mut sorted = OPS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), OPS.len());
        assert!(OPS.contains(&"run"));
        assert!(OPS.contains(&"tick"));
    }

    #[test]
    fn a_missing_id_is_an_error_not_a_default() {
        assert!(need_int(&json!({}), "id").is_err());
        assert_eq!(need_int(&json!({"id": "7"}), "id").unwrap(), 7);
        assert!(need_int(&json!({"id": 1.5}), "id").is_err());
    }

    #[test]
    fn switching_a_monitor_on_or_off_says_which_and_never_guesses() {
        assert_eq!(need_bool(&json!({"active": false}), "active").unwrap(), false);
        assert_eq!(need_bool(&json!({"active": true}), "active").unwrap(), true);
        // A caller that forgot the flag cannot silently switch the monitor on.
        let missing = need_bool(&json!({}), "active").unwrap_err();
        assert!(missing.message().contains("missing \"active\""), "{}", missing.message());
        let wrong = need_bool(&json!({"active": 1}), "active").unwrap_err();
        assert!(wrong.message().contains("true or false"), "{}", wrong.message());
    }

    #[test]
    fn a_reason_defaults_to_the_button_and_a_misspelling_is_refused() {
        assert_eq!(reason_of(&json!({})).unwrap(), Reason::Button);
        assert_eq!(reason_of(&json!({"reason": "tool"})).unwrap(), Reason::Tool);
        let error = reason_of(&json!({"reason": "just because"})).unwrap_err();
        assert!(error.message().contains("unknown reason"));
    }

    #[test]
    fn an_unknown_subcommand_names_the_ones_there_are() {
        let error = run("multiply", "{}").unwrap_err();
        assert!(error.contains("unknown subcommand"));
        assert!(error.contains("alerts"));
    }

    #[test]
    fn input_that_is_not_an_object_is_refused() {
        assert!(run("list", "[1,2]").is_err());
    }
}
