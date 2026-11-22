//! SQLite values, rows and their JSON shape — the whole bind/read boundary.
//!
//! Decisions: which JSON scalars may become bound parameters (a nested array is a
//! caller mistake, not a value), how a `Row` answers by name and by index like
//! `sqlite3.Row`, and how a value travels as JSON (`BLOB` as lowercase hex, NaN as
//! `null`). Rows are written out in *column order* because the Python bridge
//! rebuilds positional access from that text.
//!
//! It does not: execute a statement, choose a parameter order, or log anything. A
//! value is never pasted into SQL text — only into a `?`.

/// A SQLite value, as bound and as returned. Re-exported so callers never need
/// to name `rusqlite` themselves.
pub use rusqlite::types::Value as SqlValue;

use crate::error::{Result, StoreError};

/// One result row: column names plus values, indexed both ways, like
/// `sqlite3.Row`.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    cols: Vec<String>,
    vals: Vec<SqlValue>,
}

impl Row {
    pub fn new(cols: Vec<String>, vals: Vec<SqlValue>) -> Self {
        Row { cols, vals }
    }

    pub fn columns(&self) -> &[String] {
        &self.cols
    }

    pub fn values(&self) -> &[SqlValue] {
        &self.vals
    }

    pub fn len(&self) -> usize {
        self.vals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vals.is_empty()
    }

    /// First column with this name, like `row["title"]`.
    pub fn get(&self, name: &str) -> Option<&SqlValue> {
        self.cols
            .iter()
            .position(|column| column == name)
            .map(|index| &self.vals[index])
    }

    pub fn get_at(&self, index: usize) -> Option<&SqlValue> {
        self.vals.get(index)
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        match self.get(name) {
            Some(SqlValue::Text(text)) => Some(text.as_str()),
            _ => None,
        }
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        match self.get(name) {
            Some(SqlValue::Integer(value)) => Some(*value),
            Some(SqlValue::Real(value)) => Some(*value as i64),
            _ => None,
        }
    }

    pub fn real(&self, name: &str) -> Option<f64> {
        match self.get(name) {
            Some(SqlValue::Integer(value)) => Some(*value as f64),
            Some(SqlValue::Real(value)) => Some(*value),
            _ => None,
        }
    }

    /// `dict(zip(columns, values))`, the shape `db.py`'s callers already use.
    /// A repeated column name keeps the last value, exactly like the dict those
    /// callers build today.
    pub fn as_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::with_capacity(self.cols.len());
        for (name, value) in self.cols.iter().zip(self.vals.iter()) {
            map.insert(name.clone(), value_to_json(value));
        }
        serde_json::Value::Object(map)
    }
}

/// A SQLite value as JSON.
///
/// `BLOB` has no JSON type; it travels as lowercase hex so a round trip is
/// lossless and no byte is silently dropped.
pub fn value_to_json(value: &SqlValue) -> serde_json::Value {
    match value {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(number) => serde_json::Value::Number((*number).into()),
        SqlValue::Real(number) => match serde_json::Number::from_f64(*number) {
            Some(json) => serde_json::Value::Number(json),
            None => serde_json::Value::Null, // NaN and ±inf are not JSON numbers
        },
        SqlValue::Text(text) => serde_json::Value::String(text.clone()),
        SqlValue::Blob(bytes) => {
            let mut hex = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                hex.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
                hex.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
            }
            serde_json::Value::String(hex)
        }
    }
}

/// A JSON array of rows.
pub fn rows_to_json(rows: &[Row]) -> serde_json::Value {
    serde_json::Value::Array(rows.iter().map(Row::as_json).collect())
}

/// One row as JSON text, **in column order**.
///
/// `serde_json::Map` sorts its keys, and `sqlite3.Row` lets a caller ask for
/// `row[0]`. A caller that reconstructs a row from this text (the Python bridge
/// does) must get the columns in the order the statement selected them, so the
/// object is written out here instead of through a map. A statement that selects
/// the same column name twice writes two keys, which a JSON reader keeps as one —
/// the same thing `dict(zip(columns, values))` does in `db.py`'s callers.
pub fn row_to_json_text(row: &Row) -> String {
    let mut out = String::with_capacity(16 + row.len() * 24);
    out.push('{');
    for (index, (name, value)) in row.cols.iter().zip(row.vals.iter()).enumerate() {
        if index > 0 {
            out.push(',');
        }
        match serde_json::to_string(name) {
            Ok(key) => out.push_str(&key),
            Err(_) => out.push_str("\"\""),
        }
        out.push(':');
        match serde_json::to_string(&value_to_json(value)) {
            Ok(text) => out.push_str(&text),
            Err(_) => out.push_str("null"),
        }
    }
    out.push('}');
    out
}

/// Rows as JSON text, in column order (see [`row_to_json_text`]).
pub fn rows_to_json_text(rows: &[Row]) -> String {
    let mut out = String::with_capacity(2 + rows.len() * 64);
    out.push('[');
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&row_to_json_text(row));
    }
    out.push(']');
    out
}

/// One bound parameter from JSON. Scalars only: JSON has no bytes, and a nested
/// array or object is a caller mistake, not a value.
pub fn param_from_json(value: &serde_json::Value) -> Result<SqlValue> {
    Ok(match value {
        // pysqlite binds True/False as 1/0.
        serde_json::Value::Bool(flag) => SqlValue::Integer(i64::from(*flag)),
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                SqlValue::Integer(integer)
            } else if let Some(unsigned) = number.as_u64() {
                match i64::try_from(unsigned) {
                    Ok(integer) => SqlValue::Integer(integer),
                    Err(_) => SqlValue::Real(unsigned as f64),
                }
            } else if let Some(real) = number.as_f64() {
                SqlValue::Real(real)
            } else {
                return Err(StoreError::new("parameter is not a finite number"));
            }
        }
        serde_json::Value::String(text) => SqlValue::Text(text.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            return Err(StoreError::new(
                "parameters must be scalars (null, bool, number, string)",
            ))
        }
    })
}

/// A whole `params` array.
pub fn params_from_json(value: Option<&serde_json::Value>) -> Result<Vec<SqlValue>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(param_from_json(item)?);
            }
            Ok(out)
        }
        Some(_) => Err(StoreError::new("\"params\" must be an array")),
    }
}

/// `?,?,?` for `n` bound values. Placeholders only — a value never reaches the
/// SQL text.
pub fn placeholders(n: usize) -> String {
    let mut out = String::with_capacity(n.saturating_mul(2));
    for index in 0..n {
        if index > 0 {
            out.push(',');
        }
        out.push('?');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

#[test]
fn ten_kilobyte_title_and_tags_blob_survive_byte_for_byte() {
    let store = store();
    // Quotes, newlines, tabs, both slashes, emoji and mixed UTF-8, around 10 kB:
    // every kind of byte that has ever been mangled by a layer.
    let mut blob = String::new();
    while blob.len() < 10 * 1024 {
        blob.push_str("Quote\" apostrophe' back\\slash / newline\n tab\t emoji 🛰️🗞️ äöü ı İ ");
    }
    // Cut to 10 kB on a character boundary.
    while blob.len() > 10 * 1024 {
        blob.pop();
    }
    assert!(blob.len() >= 10 * 1024 - 4, "{} bytes", blob.len());

    let id = store
        .insert(
            "INSERT INTO articles(title,summary,content) VALUES(?,?,?)",
            &[text(&blob), text(&blob), text(&blob)],
        )
        .expect("insert blob");
    store
        .exec(
            "INSERT INTO article_tags(article_id,tag,weight,source) VALUES(?,?,?,?)",
            &[
                int(id),
                text(&blob),
                SqlValue::Real(1.5),
                text(&blob),
            ],
        )
        .expect("tag blob");

    let row = store
        .query_one(
            "SELECT title,summary,content FROM articles WHERE id=?",
            &[int(id)],
        )
        .expect("read")
        .expect("row");
    assert_eq!(row.text("title").expect("title"), blob.as_str());
    assert_eq!(row.text("summary").expect("summary"), blob.as_str());
    assert_eq!(row.text("content").expect("content"), blob.as_str());

    let tag = store
        .query_one(
            "SELECT tag,source FROM article_tags WHERE article_id=?",
            &[int(id)],
        )
        .expect("tag read")
        .expect("tag row");
    assert_eq!(tag.text("tag").expect("tag"), blob.as_str());
    assert_eq!(tag.text("source").expect("source"), blob.as_str());

    // And it survives the JSON wire shape unchanged, which is how the Python
    // layer will see it.
    let json = serde_json::to_string(&row.as_json()).expect("json");
    let back: serde_json::Value = serde_json::from_str(&json).expect("json back");
    assert_eq!(
        back.get("title").and_then(|value| value.as_str()),
        Some(blob.as_str())
    );
    let params = params_from_json(Some(&serde_json::Value::Array(vec![
        serde_json::Value::String(blob.clone()),
    ])))
    .expect("params");
    assert_eq!(params[0], SqlValue::Text(blob.clone()));
}

#[test]
fn params_from_json_binds_scalars_only() {
    let json: serde_json::Value =
        serde_json::from_str(r#"[1, 2.5, "text", true, false, null]"#).expect("json");
    let params = params_from_json(Some(&json)).expect("params");
    assert_eq!(params.len(), 6);
    assert_eq!(params[0], SqlValue::Integer(1));
    assert_eq!(params[1], SqlValue::Real(2.5));
    assert_eq!(params[2], SqlValue::Text("text".into()));
    assert_eq!(params[3], SqlValue::Integer(1));
    assert_eq!(params[4], SqlValue::Integer(0));
    assert_eq!(params[5], SqlValue::Null);
    let nested: serde_json::Value = serde_json::from_str(r#"[[1]]"#).expect("json");
    assert!(params_from_json(Some(&nested)).is_err());
    assert!(params_from_json(None).expect("none").is_empty());
}


#[test]
fn placeholders_never_carry_a_value() {
    assert_eq!(placeholders(0), "");
    assert_eq!(placeholders(3), "?,?,?");
    assert_eq!(placeholders(400).matches('?').count(), 400);
}


#[test]
fn row_json_text_keeps_the_order_the_statement_selected() {
    let store = store();
    // z before a: a map would sort them, and the Python bridge rebuilds
    // `sqlite3.Row`-style positional access from this order.
    let row = store
        .query_one("SELECT 'z' AS zed, 1 AS alpha, NULL AS empty_value", &[])
        .expect("row")
        .expect("one row");
    assert_eq!(row_to_json_text(&row), r#"{"zed":"z","alpha":1,"empty_value":null}"#);
    // And it is valid JSON that round-trips to the same mapping.
    let back: serde_json::Value = serde_json::from_str(&row_to_json_text(&row)).expect("json");
    assert_eq!(back.get("zed").and_then(|v| v.as_str()), Some("z"));
    assert_eq!(back.get("alpha").and_then(|v| v.as_i64()), Some(1));
    // An empty result set is still an object with a count.
    let rows = store.query("SELECT 1 WHERE 0", &[]).expect("empty");
    assert_eq!(rows_to_json_text(&rows), "[]");
}
}
