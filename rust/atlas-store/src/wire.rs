//! What the `atlas-store` binary answers on stdout, one op at a time.
//!
//! Decisions: which ops exist ([`OPS`]), what each one answers (`{"rows": …,
//! "count": n}` for a query, `{"changes": n}` for a write, `{"removed": n, …}` for
//! a prune), that a failure prints `{"error": …}` and exits 1, and that an unknown
//! or missing op names the ones that do exist. Input is always one JSON object.
//!
//! It does not: validate a field ([`crate::fields`] does), decide what SQL means,
//! or log anything. The database path is the caller's decision; this file only
//! checks that it is a path.

use std::io::{self, Read, Write};

use serde_json::{json, Value};

use crate::error::StoreError;
use crate::fields::{
    int_field, key_field, params_field, path_field, rows_field, scalar_text, sql_field, truncate,
    MAX_PARAMS,
};
use crate::params::{params_from_json, row_to_json_text, rows_to_json_text};
use crate::Store;
use crate::CHUNK;

/// Default `search` limit (`db.py: fts_query(q, limit=30)`).
const SEARCH_LIMIT: i64 = 30;

fn run() -> Result<String, StoreError> {
    let op = std::env::args().nth(1).unwrap_or_default();
    let raw = read_stdin()?;
    if raw.trim().is_empty() {
        return Err(StoreError::new("empty input (expected a JSON object)"));
    }
    let input: Value = serde_json::from_str(&raw)?;
    if !input.is_object() {
        return Err(StoreError::new("input must be a JSON object"));
    }
    let store = Store::open(path_field(&input, "db")?)?;
    match op.as_str() {
        "init" => {
            let report = store.init()?;
            Ok(answer(json!({
                "ok": true,
                "migrations_applied": report.migrations_applied,
                "migrations_skipped": report.migrations_skipped,
                "indexes": report.indexes,
                // The monitor half of the same archive (Store::init creates it):
                // reported separately so a caller can tell which half changed.
                "monitor_migrations_applied": report.monitors.migrations_applied,
                "monitor_migrations_skipped": report.monitors.migrations_skipped,
                "monitor_indexes": report.monitors.indexes,
                "monitor_unique_url_index": report.monitors.unique_url_index,
            })))
        }
        "query" => {
            let sql = sql_field(&input)?;
            let params = params_field(&input, "params")?;
            let rows = store.query(&sql, &params)?;
            Ok(rows_answer("rows", &rows))
        }
        "one" => {
            let sql = sql_field(&input)?;
            let params = params_field(&input, "params")?;
            let row = store.query_one(&sql, &params)?;
            match row {
                Some(row) => Ok(format!("{{\"row\":{}}}", row_to_json_text(&row))),
                None => Ok("{\"row\":null}".to_string()),
            }
        }
        "exec" => {
            let sql = sql_field(&input)?;
            let params = params_field(&input, "params")?;
            let changes = store.exec(&sql, &params)?;
            Ok(answer(json!({ "changes": changes })))
        }
        "many" => {
            let sql = sql_field(&input)?;
            let rows = rows_field(&input)?;
            if rows.len() > MAX_PARAMS {
                return Err(StoreError::new("too many rows in one call"));
            }
            let mut all: Vec<Vec<crate::SqlValue>> = Vec::with_capacity(rows.len());
            for row in &rows {
                let list = match row {
                    Value::Array(items) => items,
                    _ => return Err(StoreError::new("every entry of \"rows\" must be an array")),
                };
                all.push(params_from_json(Some(&Value::Array(list.clone())))?);
            }
            // One transaction (the default path), never one per row.
            let changes = store.exec_many(&sql, &all)?;
            Ok(answer(json!({ "changes": changes, "rows": all.len() })))
        }
        "meta_get" => {
            let key = key_field(&input)?;
            let value = store.get_meta(&key)?;
            Ok(answer(json!({ "value": value })))
        }
        "meta_set" => {
            let key = key_field(&input)?;
            let value = match input.get("value") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => scalar_text(other)?,
            };
            let redacted = store.set_meta(&key, &value)?;
            Ok(answer(json!({ "ok": true, "redacted": redacted })))
        }
        "search" => {
            let query = match input.get("q") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Null) | None => String::new(),
                Some(_) => return Err(StoreError::new("\"q\" must be a string")),
            };
            if query.chars().count() > 2_000 {
                return Err(StoreError::new("\"q\" is longer than 2000 characters"));
            }
            let limit = int_field(&input, "limit").unwrap_or(SEARCH_LIMIT).clamp(1, 500);
            let rows = store.search(&query, limit)?;
            Ok(rows_answer("rows", &rows))
        }
        "prune" => {
            let days = int_field(&input, "days").unwrap_or(store.retention_days());
            let chunk = int_field(&input, "chunk")
                .map(|value| value.clamp(1, 2_000) as usize)
                .unwrap_or(CHUNK);
            let report = store.prune_old_detailed(Some(days), chunk)?;
            Ok(answer(json!({
                "removed": report.removed,
                "fts_refused_batches": report.fts_refused_batches,
            })))
        }
        "" => Err(StoreError::new(format!(
            "missing op (expected one of: {})",
            OPS
        ))),
        other => Err(StoreError::new(format!(
            "unknown op '{}' (expected one of: {})",
            truncate(other, 40),
            OPS
        ))),
    }
}

/// Every op this binary answers.
const OPS: &str = "init query one exec many meta_get meta_set search prune";

/// A JSON object as text.
fn answer(value: Value) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| "{\"error\":\"unprintable\"}".to_string())
}

/// `{"<name>": [...], "count": n}` with the rows in column order.
fn rows_answer(name: &str, rows: &[crate::Row]) -> String {
    format!(
        "{{\"{}\":{},\"count\":{}}}",
        name,
        rows_to_json_text(rows),
        rows.len()
    )
}

fn emit(text: &str) {
    let mut out = io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn read_stdin() -> Result<String, StoreError> {
    let mut buffer = String::new();
    match io::stdin().read_to_string(&mut buffer) {
        Ok(_) => Ok(buffer),
        Err(err) => Err(StoreError::new(format!("cannot read stdin: {}", err))),
    }
}

/// Run the binary: argv, stdin, stdout, exit code.
pub fn serve() {
    match run() {
        Ok(text) => emit(&text),
        Err(err) => {
            emit(&answer(json!({ "error": err.message() })));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_list_is_complete() {
        for op in [
            "init", "query", "one", "exec", "many", "meta_get", "meta_set", "search", "prune",
        ] {
            assert!(OPS.split(' ').any(|known| known == op), "missing {}", op);
        }
    }
}
