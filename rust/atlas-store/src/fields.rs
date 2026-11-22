//! The fields one call carries, and how each is refused before it reaches SQL.
//!
//! Decisions: what a database path may look like (non-empty, no control
//! characters, bounded), what a statement may look like (non-empty, bounded, no
//! NUL), how many parameters and rows one call may bind, and how a JSON scalar
//! becomes text the way `str()` would. Every refusal names the field.
//!
//! It does not: open the database, run the statement or assemble SQL text — a
//! value only ever becomes a `?`, so a caller cannot smuggle a statement through a
//! parameter.

use serde_json::Value;

use crate::error::StoreError;
use crate::params::params_from_json;

/// Longest statement accepted. A bridge that hands over a megabyte of SQL is
/// broken, not clever.
const MAX_SQL_BYTES: usize = 256 * 1024;

/// Longest database path accepted.
const MAX_PATH_BYTES: usize = 4_096;

/// Most bound values accepted in one call.
pub(crate) const MAX_PARAMS: usize = 32_768;

pub(crate) fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The database path, validated. Control characters are refused: a path with a
/// newline in it is a smuggled argument, not a filename.
pub(crate) fn path_field(input: &Value, name: &str) -> Result<String, StoreError> {
    let text = match input.get(name) {
        Some(Value::String(text)) => text.clone(),
        _ => {
            return Err(StoreError::new(format!(
                "missing \"{}\" (the database path)",
                name
            )))
        }
    };
    if text.trim().is_empty() {
        return Err(StoreError::new(format!("\"{}\" is empty", name)));
    }
    if text.len() > MAX_PATH_BYTES {
        return Err(StoreError::new(format!("\"{}\" is too long", name)));
    }
    if text.chars().any(|ch| ch.is_control()) {
        return Err(StoreError::new(format!(
            "\"{}\" contains control characters",
            name
        )));
    }
    Ok(text)
}

pub(crate) fn sql_field(input: &Value) -> Result<String, StoreError> {
    let sql = match input.get("sql") {
        Some(Value::String(sql)) => sql.clone(),
        _ => return Err(StoreError::new("missing \"sql\" string")),
    };
    if sql.trim().is_empty() {
        return Err(StoreError::new("\"sql\" is empty"));
    }
    if sql.len() > MAX_SQL_BYTES {
        return Err(StoreError::new("\"sql\" is too long"));
    }
    if sql.contains('\0') {
        return Err(StoreError::new("\"sql\" contains a NUL byte"));
    }
    Ok(sql)
}

pub(crate) fn params_field(input: &Value, name: &str) -> Result<Vec<crate::SqlValue>, StoreError> {
    let params = params_from_json(input.get(name))?;
    if params.len() > MAX_PARAMS {
        return Err(StoreError::new(format!(
            "\"{}\" has more than {} values",
            name, MAX_PARAMS
        )));
    }
    Ok(params)
}

pub(crate) fn rows_field(input: &Value) -> Result<Vec<Value>, StoreError> {
    match input.get("rows") {
        Some(Value::Array(rows)) => Ok(rows.clone()),
        // A caller that sent `params` as a list of lists meant `rows`.
        Some(Value::Null) | None => match input.get("params") {
            Some(Value::Array(items)) if items.iter().all(|item| item.is_array()) => {
                Ok(items.clone())
            }
            Some(Value::Null) | None => Ok(Vec::new()),
            Some(_) => Err(StoreError::new(
                "\"rows\" must be an array of parameter arrays",
            )),
        },
        Some(_) => Err(StoreError::new(
            "\"rows\" must be an array of parameter arrays",
        )),
    }
}

pub(crate) fn key_field(input: &Value) -> Result<String, StoreError> {
    let key = match input.get("key") {
        Some(Value::String(key)) => key.clone(),
        _ => return Err(StoreError::new("missing \"key\" string")),
    };
    if key.is_empty() {
        return Err(StoreError::new("\"key\" is empty"));
    }
    if key.chars().count() > 200 {
        return Err(StoreError::new("\"key\" is longer than 200 characters"));
    }
    Ok(key)
}

pub(crate) fn int_field(input: &Value, name: &str) -> Option<i64> {
    match input.get(name) {
        Some(Value::Number(number)) => number.as_i64().or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite())
                .map(|value| value as i64)
        }),
        Some(Value::String(text)) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// A non-string `meta_set` value, written the way `str()` would.
pub(crate) fn scalar_text(value: &Value) -> Result<String, StoreError> {
    Ok(match value {
        Value::Bool(flag) => {
            if *flag {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(_) | Value::Object(_) => {
            return Err(StoreError::new("\"value\" must be a scalar"));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_and_sql_validation_rejects_smuggling() {
        let ok: Value = serde_json::from_str(r#"{"db":"/tmp/atlas.db"}"#).expect("json");
        assert_eq!(path_field(&ok, "db").expect("path"), "/tmp/atlas.db");
        let empty: Value = serde_json::from_str(r#"{"db":"  "}"#).expect("json");
        assert!(path_field(&empty, "db").is_err());
        let newline: Value = serde_json::from_str(r#"{"db":"/tmp/a\nb.db"}"#).expect("json");
        assert!(path_field(&newline, "db").is_err());

        let sql: Value = serde_json::from_str(r#"{"sql":"SELECT 1"}"#).expect("json");
        assert_eq!(sql_field(&sql).expect("sql"), "SELECT 1");
        let blank: Value = serde_json::from_str(r#"{"sql":"   "}"#).expect("json");
        assert!(sql_field(&blank).is_err());
        let nul: Value = serde_json::from_str(r#"{"sql":"SELECT 1\u0000"}"#).expect("json");
        assert!(sql_field(&nul).is_err());
    }

    #[test]
    fn rows_field_accepts_the_documented_shapes() {
        let rows: Value = serde_json::from_str(r#"{"rows":[[1,"a"],[2,"b"]]}"#).expect("json");
        assert_eq!(rows_field(&rows).expect("rows").len(), 2);
        let as_params: Value = serde_json::from_str(r#"{"params":[[1],[2]]}"#).expect("json");
        assert_eq!(rows_field(&as_params).expect("params").len(), 2);
        let wrong: Value = serde_json::from_str(r#"{"rows":"nope"}"#).expect("json");
        assert!(rows_field(&wrong).is_err());
        let empty: Value = serde_json::from_str(r#"{}"#).expect("json");
        assert!(rows_field(&empty).expect("empty").is_empty());
    }

    #[test]
    fn ints_and_scalars_read_like_python() {
        let object: Value = serde_json::from_str(r#"{"limit":10,"days":"21"}"#).expect("json");
        assert_eq!(int_field(&object, "limit"), Some(10));
        assert_eq!(int_field(&object, "days"), Some(21));
        assert_eq!(int_field(&object, "missing"), None);
        assert_eq!(
            scalar_text(&serde_json::json!(true)).expect("bool"),
            "1".to_string()
        );
        assert!(scalar_text(&serde_json::json!({"a": 1})).is_err());
    }

    #[test]
    fn many_rows_bind_through_params_not_text() {
        // The values below must arrive as bound values; the statement text is
        // never assembled from input.
        let input: Value =
            serde_json::from_str(r#"{"rows":[["x'); DROP TABLE meta; --","v"]]}"#).expect("json");
        let rows = rows_field(&input).expect("rows");
        let mut bound: Vec<Vec<crate::SqlValue>> = Vec::new();
        for row in &rows {
            bound.push(params_from_json(Some(row)).expect("params"));
        }
        assert_eq!(bound.len(), 1);
        assert_eq!(
            bound[0][0],
            crate::SqlValue::Text("x'); DROP TABLE meta; --".to_string())
        );
    }

    #[test]
    fn object_input_is_what_every_op_takes() {
        let object = serde_json::json!({"db": ":memory:"});
        assert!(object.is_object());
        assert_eq!(path_field(&object, "db").expect("path"), ":memory:");
    }
}
