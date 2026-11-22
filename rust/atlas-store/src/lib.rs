//! ATLAS persistence layer — the Rust port of `server/db.py`, one function for
//! one function.
//!
//! Everything ATLAS knows lives in one SQLite file (`data/atlas.db`): articles,
//! entities, edges, threads, topics, full text. This crate owns every statement
//! that touches it, so a second copy of a security or retention rule cannot
//! drift away from the first.
//!
//! # The map
//!
//! `lib.rs` is the [`Store`] itself — one connection behind one lock, the pragmas,
//! the statement layer every domain uses, and the re-exports that keep the public
//! paths stable. The statements themselves live one concern per file:
//!
//! * [`schema`] — the DDL, the migration list, and [`Init`] (what `init` changed).
//! * [`articles`] — is this story already stored, is it stored twice, which tables
//!   point at an article.
//! * [`search`] — FTS5 matching and the `LIKE` fallback that keeps it answering.
//! * [`meta`] — `meta` key/values, and the one credential check in the store path.
//! * [`prune`] — retention: delete by cutoff, in batches, counting FTS refusals.
//! * [`backup`] — `VACUUM INTO`, refused if the destination already exists.
//! * [`params`] — values, rows and their JSON shape (the whole bind/read boundary).
//! * [`error`] — [`StoreError`] and the scrub that makes a message safe to log.
//! * [`clock`] — the two timestamps ATLAS stores, without a `chrono` crate.
//! * [`norm`] — the headline key that decides whether two titles are one story.
//! * [`perms`] — `data/` is 0700, the database files are 0600.
//! * [`fields`] — the JSON fields one call carries, and how each is refused.
//! * [`monitors`] — the monitor tables, the monitor row, the check log (the SQL
//!   `server/monitor.py` kept inline), and the DDL every one of them needs.
//! * [`alerts`] — the alert row, its duplicate rule, the seen flags and the
//!   "already judged" marks.
//! * [`requests`] — the request queue in its own file (`data/requests.db`): what a
//!   click writes and the agent claims. Not part of the archive, and the one
//!   database the window may write.
//! * `wire` — the JSON answer the `atlas-store` binary prints for one op.
//!
//! # What is deliberately identical to `server/db.py`
//!
//! * **Schema and migrations.** [`SCHEMA`], [`MIGRATIONS`] and
//!   [`POST_MIGRATION_INDEXES`] are the same statements in the same order.
//!   Migrations are `ALTER TABLE … ADD COLUMN`, so re-running them on an
//!   up-to-date database fails and is swallowed — that *is* the version-less
//!   migration scheme, and [`Store::init`] reports how many statements were
//!   applied versus already there.
//! * **Pragmas.** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`,
//!   and a 5 000 ms busy timeout — `sqlite3.connect()`'s default `timeout=5.0`,
//!   which `db.py` relies on without writing it down.
//! * **One connection, one lock.** `db.py` keeps a module-level connection with
//!   an `RLock` and `check_same_thread=False`; [`Store`] is a single connection
//!   behind a `Mutex`, so callers see the same serialisation.
//! * **Parameterised SQL only.** Values are always bound. The only text ever
//!   assembled into a statement here is a list of `?` placeholders, sized by a
//!   count — never a value. rusqlite also refuses a second statement in one
//!   call, which is what `sqlite3.execute()` does.
//!
//! # Where it cannot be identical, and why
//!
//! * `norm_title` needs `unicodedata.normalize("NFKD")`, which the standard
//!   library does not have and this workspace must build offline: the
//!   decomposition is a written-out folding table for Latin letters with
//!   diacritics, the fullwidth forms and the NFKD ligatures. See [`norm`]. A
//!   script outside that table does not decompose, so a duplicate pair written in
//!   it is missed rather than merged — a recall loss, not a wrong merge.
//! * `set_meta` asks `atlas-guard` whether a key/value pair carries credential
//!   material, instead of carrying a copy of `config.py`'s secret-name regex. The
//!   vocabulary stays in one place; see [`Store::set_meta`].
//! * `backup` uses `VACUUM INTO`, which needs the destination to be absent,
//!   rather than `sqlite3_backup` that would overwrite — a backup never silently
//!   replaces an older one.
//! * Two of `db.py`'s indexes are **retired** ([`RETIRED_INDEXES`]): `idx_links_a`
//!   and `idx_edges_a` repeat the first column of a primary key that already
//!   starts with it, so they answered no lookup the primary key could not and cost
//!   a second B-tree on the two most-written tables (`SCHEMA`'s doc comment has the
//!   measurement: 60 000 `article_links` writes 17.1/26.1/29.0 ms with it,
//!   6.4/5.8/7.8 ms without). An archive that holds them drops them on the next
//!   `init`; no row and no result changes.
//! * `prune_old` keeps `db.py`'s statement for the full-text rows but does not
//!   let it stop the prune: FTS5 refuses to delete a row it never indexed (the
//!   live archive has 1 435 articles and 1 199 indexed documents), and `db.py`
//!   raises there and leaves the archive un-pruned. The refusal is atomic, so the
//!   batch is counted in [`Pruned::fts_refused_batches`] and the prune carries on.
//!   `db.py`'s one-number signature is preserved by [`Store::prune_old`].
//!
//! # Never in a log
//!
//! No article text, summary, prompt or page content is ever printed by this
//! crate. Errors carry a reason scrubbed through `atlas-guard` and truncated;
//! counters carry counts.

#![forbid(unsafe_code)]

pub mod articles;
pub mod backup;
pub mod clock;
pub mod error;
pub mod fields;
pub mod meta;
pub mod norm;
pub mod params;
pub mod perms;
pub mod alerts;
pub mod briefings;
pub mod monitors;
pub mod prune;
pub mod requests;
pub mod schema;
pub mod search;
pub mod wire;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::types::ValueRef;
use rusqlite::{params_from_iter, Connection};

// Every path this crate published before the split, re-exported at the crate root
// so no caller needs a single edit.
pub use articles::DUPE_WINDOW_HOURS;
pub use briefings::Briefing;
pub use clock::{iso_ago, iso_from_epoch, now_iso, parse_epoch};
pub use error::{scrub, Result, StoreError};
pub use norm::norm_title;
pub use params::{
    param_from_json, params_from_json, placeholders, row_to_json_text, rows_to_json,
    rows_to_json_text, value_to_json, Row, SqlValue,
};
pub use perms::{PermissionReport, PermInfo, DATA_MODE, FILE_MODE, POSIX};
pub use prune::Pruned;
pub use requests::{Request, Requests};
pub use schema::{Init, MIGRATIONS, POST_MIGRATION_INDEXES, RETIRED_INDEXES, SCHEMA};

/// Rows per `DELETE … IN (…)`: keeps the variable count and the write lock
/// small. (`db.py: CHUNK`.)
pub const CHUNK: usize = 400;

/// Busy timeout, `sqlite3.connect()`'s default `timeout=5.0`, which `db.py`
/// inherits without stating it.
pub const BUSY_TIMEOUT_MS: u64 = 5_000;

/// What `db.py: retention_days()` defaults to.
pub const RETENTION_DAYS: i64 = 21;

/// What `db.py: window_hours()` defaults to.
pub const WINDOW_HOURS: i64 = 72;

/// The database: one connection, one lock, every statement ATLAS makes.
pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
    retention_days: i64,
    window_hours: i64,
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    ///
    /// Sets the same pragmas as `db.py: connect()` and makes `data/` 0700 and the
    /// database files 0600 on POSIX. A database found world-readable is reported
    /// through [`Store::permission_report`], never silently accepted and never a
    /// crash: a library does not write to a log.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() {
            return Err(StoreError::new("database path is empty"));
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                perms::ensure_dir(parent);
            }
        }
        if !path.exists() {
            // Create it 0600 ourselves, from the first byte: SQLite would use the
            // umask and leave a fresh archive world-readable for as long as it
            // lives. On Windows `atlas-platform` hands back the plain options,
            // because the mode call does not exist there.
            let _ = perms::create_options().open(&path);
        }
        let conn = Connection::open(&path).map_err(StoreError::from)?;
        let store = Store {
            conn: Mutex::new(conn),
            path,
            retention_days: RETENTION_DAYS,
            window_hours: WINDOW_HOURS,
        };
        store.apply_pragmas()?;
        store.tighten();
        Ok(store)
    }

    /// An in-memory database with the same pragmas minus WAL (SQLite keeps no
    /// WAL for `:memory:`). Tests and one-shot tools only.
    pub fn open_memory() -> Result<Store> {
        let conn = Connection::open_in_memory().map_err(StoreError::from)?;
        let store = Store {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
            retention_days: RETENTION_DAYS,
            window_hours: WINDOW_HOURS,
        };
        store.apply_pragmas()?;
        Ok(store)
    }

    fn is_memory(&self) -> bool {
        self.path.to_string_lossy() == ":memory:"
    }

    /// The connection's posture, set once per open.
    ///
    /// Four of these are `db.py`'s (`journal_mode`, `synchronous`,
    /// `foreign_keys`, `busy_timeout`). Two are additions this port needs because
    /// the archive is a file this process opens read-write on every boot:
    ///
    /// * `trusted_schema=OFF`. With it ON (SQLite's default) a *schema object* —
    ///   a view, a trigger, a `CHECK` constraint, a generated column, an index
    ///   expression — may call any function the connection knows. No object in
    ///   this crate's DDL does, so the archive is declaring a property it already
    ///   has: a `sqlite_master` somebody got to (a restored backup, a file copied
    ///   in, a future statement that ever takes text) reads as data instead of
    ///   being able to execute. This is defence in depth, not a live injection
    ///   fix: every statement in this crate binds its values (see the module
    ///   docs), so nothing today hands the connection text it did not write.
    /// * `defensive` (`SQLITE_DBCONFIG_DEFENSIVE`, which has no `PRAGMA`
    ///   spelling). It makes `PRAGMA writable_schema=ON` a no-op and refuses
    ///   direct writes to `sqlite_master` and to virtual-table shadow tables, so
    ///   text that did reach this connection could corrupt *rows* at worst and
    ///   never the schema the next `init` runs from. The FTS5 commands this crate
    ///   uses (`'rebuild'`, `'integrity-check'`) and its row deletes go through
    ///   the fts5 module rather than through shadow-table SQL, so they keep
    ///   working — `defensive_mode_refuses_a_schema_rewrite_and_keeps_fts_working`
    ///   measures both halves.
    ///
    /// `query_only` is deliberately *not* set: the archive is written.
    fn apply_pragmas(&self) -> Result<()> {
        let conn = self.lock();
        // `PRAGMA journal_mode` answers with the mode it ended up in, which is
        // exactly what db.py asks for and what it gets on a file database.
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(StoreError::from)?;
        let mode = mode.to_ascii_lowercase();
        if mode != "wal" && !self.is_memory() {
            return Err(StoreError::new(format!(
                "journal_mode stayed '{}' - a file database must be WAL",
                mode
            )));
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")
            .map_err(StoreError::from)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(StoreError::from)?;
        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .map_err(StoreError::from)?;
        if foreign_keys != 1 {
            return Err(StoreError::new("foreign_keys did not turn on"));
        }
        conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(StoreError::from)?;
        // Above: `db.py`'s four. Below: this port's two, read back so a build of
        // SQLite that ignored them fails here instead of in a report.
        conn.execute_batch("PRAGMA trusted_schema=OFF;")
            .map_err(StoreError::from)?;
        let trusted_schema: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
            .map_err(StoreError::from)?;
        if trusted_schema != 0 {
            return Err(StoreError::new("trusted_schema did not turn off"));
        }
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
            .map_err(StoreError::from)?;
        if !conn
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .map_err(StoreError::from)?
        {
            return Err(StoreError::new("defensive mode did not turn on"));
        }
        Ok(())
    }

    /// 0700 the data directory, 0600 the database files. Returns what changed.
    pub fn tighten(&self) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                perms::ensure_dir(parent);
            }
        }
        for file in self.files() {
            if file.is_file() && perms::ensure_file(&file, FILE_MODE) {
                changed.push(file);
            }
        }
        changed
    }

    /// The database file, its `-wal` and its `-shm`.
    pub fn files(&self) -> Vec<PathBuf> {
        vec![self.path.clone(), backup::sibling(&self.path, "-wal"), backup::sibling(&self.path, "-shm")]
    }

    /// Modes of `data/` and the database files (`db.py: permission_report`).
    pub fn permission_report(&self) -> PermissionReport {
        let mut entries = Vec::new();
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                entries.push(perms::permissions(parent));
            }
        }
        for file in self.files() {
            entries.push(perms::permissions(&file));
        }
        let exposed: Vec<String> = entries
            .iter()
            .filter(|entry| entry.exists && entry.world_readable)
            .map(|entry| entry.path.clone())
            .collect();
        let ok = entries.iter().filter(|entry| entry.exists).all(|entry| entry.ok);
        PermissionReport {
            posix: POSIX,
            entries,
            exposed,
            ok,
        }
    }

    /// Schema plus migrations, idempotently (`db.py: init`), and the monitor half
    /// of the same archive.
    ///
    /// One call, one complete shape. The app's boot path calls this and nothing
    /// else, while `/api/alerts`, `/api/monitors` and the check log read
    /// `alerts`/`monitors`/`monitor_seen`/`monitor_runs`; before this, those tables
    /// came from a second, older copy of the DDL in the server crate, so a fresh
    /// archive had no `monitor_runs` and an `alerts` table with no `terms` column
    /// until a monitor check happened to run. The DDL lives in
    /// [`crate::monitors`] and the migration rule in [`crate::schema`]; this is
    /// only the order they run in.
    pub fn init(&self) -> Result<Init> {
        let mut report = {
            let conn = self.lock();
            crate::schema::apply(&conn)?
        };
        // After the guard above is dropped: `init_monitors` and `briefings_init` take
        // the same lock, and a `Mutex` here is not re-entrant.
        report.monitors = self.init_monitors()?;
        // The briefing shelf ([`crate::briefings`]): created for every archive, because a
        // reader may tolerate its absence but nothing should have to.
        self.briefings_init()?;
        self.tighten();
        Ok(report)
    }

    /// The database path this store opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How long an article may stay in the archive. `db.py` reads this from
    /// `config.json`; the caller passes it in (`storecore.py` still reads
    /// config), because a library must not read the app's config file.
    pub fn set_retention_days(&mut self, days: i64) {
        self.retention_days = days.max(1);
    }

    pub fn retention_days(&self) -> i64 {
        self.retention_days
    }

    /// The freshness window every query shares (`db.py: window_hours`), set by
    /// the caller for the same reason.
    pub fn set_window_hours(&mut self, hours: i64) {
        self.window_hours = hours.max(1);
    }

    pub fn window_hours(&self) -> i64 {
        self.window_hours
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic elsewhere must not turn the archive into a poisoned lock for
        // the rest of the process; SQLite rolls an open transaction back on
        // unwind, so the connection itself is still consistent.
        match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    // -- generic statements ------------------------------------------------

    /// Every row of one statement (`db.py: query`).
    pub fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(sql).map_err(StoreError::from)?;
        let cols: Vec<String> = stmt.column_names().into_iter().map(|c| c.to_string()).collect();
        let mut rows = stmt
            .query(params_from_iter(params.iter()))
            .map_err(StoreError::from)?;
        let mut out: Vec<Row> = Vec::new();
        while let Some(row) = rows.next().map_err(StoreError::from)? {
            let mut vals = Vec::with_capacity(cols.len());
            for index in 0..cols.len() {
                let raw: ValueRef<'_> = row.get_ref(index).map_err(StoreError::from)?;
                vals.push(SqlValue::try_from(raw).map_err(StoreError::from)?);
            }
            out.push(Row::new(cols.clone(), vals));
        }
        Ok(out)
    }

    /// The first row, or `None` (`db.py: query_one`).
    pub fn query_one(&self, sql: &str, params: &[SqlValue]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    /// One statement, committed (`db.py: exec`). Returns the changed-row count.
    ///
    /// A statement that returns rows is drained rather than refused, because
    /// `db.py: exec` has always accepted one.
    pub fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<usize> {
        let conn = self.lock();
        Self::exec_on(&conn, sql, params)
    }

    fn exec_on(conn: &Connection, sql: &str, params: &[SqlValue]) -> Result<usize> {
        let mut stmt = conn.prepare(sql).map_err(StoreError::from)?;
        // A statement that returns rows changes none, and `sqlite3_changes()`
        // would still report the previous statement's count.
        let returns_rows = stmt.column_count() > 0;
        {
            let mut rows = stmt
                .query(params_from_iter(params.iter()))
                .map_err(StoreError::from)?;
            while rows.next().map_err(StoreError::from)?.is_some() {}
        }
        Ok(if returns_rows {
            0
        } else {
            conn.changes() as usize
        })
    }

    /// `INSERT` then `last_insert_rowid()` (`db.py` hands back the cursor; this
    /// returns the id every caller actually wants).
    pub fn insert(&self, sql: &str, params: &[SqlValue]) -> Result<i64> {
        let conn = self.lock();
        Self::exec_on(&conn, sql, params)?;
        Ok(conn.last_insert_rowid())
    }

    /// Many rows of one statement — **one transaction**, which is the default
    /// path: 2 000 articles cost one fsync instead of 2 000. `rows` holds one
    /// parameter list per execution (`db.py: exec_many`).
    pub fn exec_many(&self, sql: &str, rows: &[Vec<SqlValue>]) -> Result<usize> {
        self.exec_many_chunked(sql, rows, 0)
    }

    /// Same, in transactions of at most `chunk` rows (`0` = one transaction). For
    /// an import big enough that holding the write lock for the whole of it would
    /// starve a reader.
    pub fn exec_many_chunked(
        &self,
        sql: &str,
        rows: &[Vec<SqlValue>],
        chunk: usize,
    ) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0); // db.py: `if not seq: return None`
        }
        let step = if chunk == 0 { rows.len() } else { chunk.max(1) };
        let mut total = 0usize;
        for part in rows.chunks(step) {
            total += self.in_transaction(|conn| {
                let mut stmt = conn.prepare(sql).map_err(StoreError::from)?;
                let mut changed = 0usize;
                for params in part {
                    stmt.execute(params_from_iter(params.iter()))
                        .map_err(StoreError::from)?;
                    changed += conn.changes() as usize;
                }
                Ok(changed)
            })?;
        }
        Ok(total)
    }

    /// `insert` for many rows, one transaction (see [`Store::exec_many`]).
    pub fn insert_many(&self, sql: &str, rows: &[Vec<SqlValue>]) -> Result<usize> {
        self.exec_many(sql, rows)
    }

    /// Run `body` inside one transaction, committing on `Ok` and rolling back on
    /// `Err` or panic.
    ///
    /// `body` gets the transaction's connection; it must not call back into
    /// [`Store`] (the lock is held).
    pub fn in_transaction<T, F>(&self, body: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(StoreError::from)?;
        let out = body(&tx)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(out)
    }

}
