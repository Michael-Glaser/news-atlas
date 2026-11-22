//! Opening the archive: read-only for verification, through `atlas-store` for any
//! write.
//!
//! Decisions: that `verify` cannot touch the file (`SQLITE_OPEN_READ_ONLY` plus
//! `query_only`), that a row read here is an [`atlas_store::Row`] built by the same
//! value boundary `atlas-store::params` uses, and that a write opens the archive
//! through [`atlas_store::Store`] so the pragmas, the schema and the statement
//! layer are the store's, not a second copy.
//!
//! It does not: create a database (`Store::open` does, and `verify` refuses a
//! missing file instead of creating an empty one), run a migration
//! (`Store::init` is the app's boot step, not this crate's), or write anything on
//! the read-only path.

use std::path::{Path, PathBuf};

use atlas_store::{Row, SqlValue};
use rusqlite::types::ValueRef;
use rusqlite::{params_from_iter, Connection, OpenFlags};

use crate::error::{MigrateError, Result};

/// One open archive, read-only.
///
/// A verification run reads through this and nothing else, so "`--dry-run` changes
/// nothing" is a property of the connection flag, not of a promise in a comment.
pub struct Archive {
    conn: Connection,
    path: PathBuf,
}

impl Archive {
    /// Open `path` without the ability to write it.
    ///
    /// The file must exist: an empty database verifies as an empty database, which
    /// would be a report nobody asked for.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Archive> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return Err(MigrateError::new(format!(
                "no database at {} - pass --db with the archive to verify",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| {
            MigrateError::new(format!(
                "{} cannot be opened read-only: {}",
                path.display(),
                error
            ))
        })?;
        // Belt: even a statement that tries to write fails here.
        conn.execute_batch("PRAGMA query_only=ON;")
            .map_err(MigrateError::from)?;
        Ok(Archive { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every row of one statement.
    pub fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare(sql).map_err(MigrateError::from)?;
        let cols: Vec<String> = stmt
            .column_names()
            .into_iter()
            .map(|name| name.to_string())
            .collect();
        let mut rows = stmt
            .query(params_from_iter(params.iter()))
            .map_err(MigrateError::from)?;
        let mut out: Vec<Row> = Vec::new();
        while let Some(row) = rows.next().map_err(MigrateError::from)? {
            let mut vals: Vec<SqlValue> = Vec::with_capacity(cols.len());
            for index in 0..cols.len() {
                let raw: ValueRef<'_> = row.get_ref(index).map_err(MigrateError::from)?;
                vals.push(SqlValue::try_from(raw).map_err(|error| {
                    MigrateError::new(format!("unreadable value: {}", error))
                })?);
            }
            out.push(Row::new(cols.clone(), vals));
        }
        Ok(out)
    }

    /// The first row, or `None`.
    pub fn query_one(&self, sql: &str, params: &[SqlValue]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    /// One integer from one row (`COUNT(*)`, `SUM(...)`); `0` when the statement
    /// answered nothing.
    pub fn count(&self, sql: &str, params: &[SqlValue]) -> Result<i64> {
        let row = self.query_one(sql, params)?;
        Ok(row
            .as_ref()
            .and_then(|row| row.values().first().and_then(|value| match value {
                SqlValue::Integer(number) => Some(*number),
                _ => None,
            }))
            .unwrap_or(0))
    }

    /// `PRAGMA table_info(t)` — the column names of one table, in order.
    pub fn columns(&self, table: &str) -> Result<Vec<String>> {
        if !crate::queries::TABLES.contains(&table) {
            return Err(MigrateError::new(format!("unknown table: {}", table)));
        }
        let rows = self.query(&format!("PRAGMA table_info(\"{}\")", table), &[])?;
        Ok(rows
            .iter()
            .filter_map(|row| row.text("name").map(|name| name.to_string()))
            .collect())
    }

    /// Does `table` exist at all? A database without `articles` is not an archive.
    pub fn has_table(&self, table: &str) -> Result<bool> {
        let row = self.query_one(
            crate::queries::TABLE_EXISTS,
            &[SqlValue::Text(table.to_string())],
        )?;
        Ok(row.is_some())
    }
}

/// Open the archive for writing, through `atlas-store`.
///
/// `Store::open` creates `data/` 0700 and the database 0600 and sets the pragmas
/// every writer in ATLAS shares; `init` is deliberately **not** called, because a
/// carry-over examines the schema it finds instead of defining a new one.
pub fn open_write(path: impl AsRef<Path>) -> Result<atlas_store::Store> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(MigrateError::new(format!(
            "no database at {} - nothing to carry",
            path.display()
        )));
    }
    Ok(atlas_store::Store::open(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_refused_instead_of_created() {
        let path = std::env::temp_dir().join("atlas-migrate-open-missing.db");
        let _ = std::fs::remove_file(&path);
        let refused = Archive::open_read_only(&path);
        assert!(refused.is_err(), "verify must not create a database");
        assert!(!path.exists(), "and must not leave one behind");
    }

    #[test]
    fn the_read_only_connection_refuses_a_write() {
        let path = crate::fixture::empty_database("open-ro");
        let archive = Archive::open_read_only(&path).expect("open");
        let written = archive.query("DELETE FROM articles", &[]);
        assert!(written.is_err(), "query_only must refuse a write");
        assert_eq!(archive.count("SELECT COUNT(*) c FROM articles", &[]).unwrap_or(-1), 0);
    }
}
