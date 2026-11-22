//! A consistent copy of the live archive, written by `VACUUM INTO`.
//!
//! Decisions: where a default backup goes (`data/backups/atlas-<UTC>.db`), that an
//! existing destination is *refused* rather than overwritten, and that the new
//! file is 0600. `VACUUM INTO` folds the WAL in and needs no `backup` feature.
//!
//! It does not: run on a schedule (nothing backs up on its own), prune anything,
//! or verify the copy afterwards.

use std::path::{Path, PathBuf};

use crate::clock;
use crate::error::{Result, StoreError};
use crate::params::SqlValue;
use crate::perms::{self, FILE_MODE};
use crate::Store;

impl Store {
    /// A consistent copy of the live database (WAL folded in), 0600.
    ///
    /// Explicit only — nothing backs up on its own, and a backup never silently
    /// replaces an older file: an existing destination is refused. `dest`
    /// defaults to `data/backups/atlas-<UTC timestamp>.db`.
    pub fn backup(&self, dest: Option<&Path>) -> Result<PathBuf> {
        let target = match dest {
            Some(path) => path.to_path_buf(),
            None => {
                let directory = match self.path.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                    _ => PathBuf::from("."),
                };
                directory
                    .join("backups")
                    .join(format!("atlas-{}.db", compact_stamp()))
            }
        };
        if target.exists() {
            return Err(StoreError::new(format!(
                "backup destination already exists: {}",
                target.display()
            )));
        }
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                perms::ensure_dir(parent);
            }
        }
        // VACUUM INTO writes one self-contained file in journal_mode=DELETE: the
        // same thing db.py's `backup()` + `PRAGMA journal_mode=DELETE` produces,
        // without the `backup` feature.
        self.exec(
            "VACUUM INTO ?",
            &[SqlValue::Text(target.to_string_lossy().to_string())],
        )?;
        perms::ensure_file(&target, FILE_MODE);
        Ok(target)
    }
}

/// `atlas.db` + `-wal` → `atlas.db-wal`, without assuming a separator.
pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.to_path_buf().into_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `20261105-100000` in UTC, for backup filenames (`db.py` uses `strftime`).
fn compact_stamp() -> String {
    let secs = clock::epoch_secs_now();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = clock::civil_from_days(days);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        year,
        month,
        day,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::clock::now_iso;

#[test]
fn backup_writes_one_self_contained_file() {
    let dir = temp_dir("backup");
    let db = dir.join("atlas.db");
    let store = Store::open(&db).expect("open");
    store.init().expect("init");
    insert_article(&store, "A story", "AP", now_iso());
    let dest = dir.join("copy.db");
    let path = store.backup(Some(&dest)).expect("backup");
    assert_eq!(path, dest);
    assert!(dest.is_file());
    // No WAL next to the copy: VACUUM INTO writes journal_mode=DELETE, before
    // anything opens the copy (opening it in WAL mode would create one).
    let mut wal = dest.clone().into_os_string();
    wal.push("-wal");
    assert!(!PathBuf::from(wal).exists(), "backup left a WAL file");
    // The copy opens on its own and holds the row.
    let copy = Store::open(&dest).expect("open copy");
    assert_eq!(
        copy.query("SELECT COUNT(*) c FROM articles", &[])
            .expect("count")[0]
            .int("c"),
        Some(1)
    );
    // A second backup to the same path is refused, not silently overwritten.
    assert!(store.backup(Some(&dest)).is_err());
    drop(copy);
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

}
