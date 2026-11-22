//! The request queue: one small table in its own file, `data/requests.db`.
//!
//! The window cannot start a research pass. It writes no file, spawns nothing and
//! listens on nothing, and the archive is opened read-only — so a click has no way
//! to reach the agent. This module is the seam: a row is a request, the window
//! pushes one, the always-on child claims it. The queue is a *separate database*
//! from `data/atlas.db` on purpose, because the whole point is that the window
//! never holds a writable handle on the archive; `requests.db` is the one file it
//! may write, and it holds no article.
//!
//! Decisions in this file:
//!
//! * `kind` is a closed set ([`KINDS`]) checked here, so a typo in a caller is a
//!   refusal at push time rather than a row nothing will ever claim.
//! * `payload` is text, capped at [`MAX_PAYLOAD_BYTES`] and shaped like one JSON
//!   object. The store does not parse it: what a `research` payload *means* is the
//!   agent's decision, and a store that read it would be a second place to change
//!   when the agent grows a field.
//! * a payload that carries a **credential** is refused, not rewritten
//!   ([`CREDENTIAL_REFUSAL`]). The verdict is `atlas-guard`'s redaction, asked in
//!   the shape it scans; the sentence names the fix. A rewritten payload would hand
//!   the agent a request the reader did not write.
//! * a claim is one statement ([`CLAIM_SQL`]) with `RETURNING`, so two claimers —
//!   two connections, two processes — can never be handed the same row: the
//!   `UPDATE` takes the write lock, the sub-select runs inside it, and the ids it
//!   rewrites are exactly the rows it returns.
//! * a claim is a lease, not a lock. A child that dies holding one leaves
//!   `claimed_at` behind, and [`Requests::claim_due`] takes the row again once
//!   `claimed_at` is older than `stale_after_secs` — the queue cannot lose a
//!   request to a crash.
//! * `done_at` is final: a finished row is invisible to the claim statement, so a
//!   late claimer cannot redo work that already happened.
//!
//! It does not: run anything, decide what a request means, retry a failure, or
//! know which kinds exist beyond the set above. The loop that claims rows lives in
//! the agent; the launcher supervises it.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

use crate::clock::{iso_from_epoch, now_iso, parse_epoch};
use crate::error::{Result, StoreError};
use crate::{perms, BUSY_TIMEOUT_MS};

/// The one table. Spelled out rather than generated: a queue this small that has
/// to be migrated is a queue that has outgrown a queue.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS requests (id INTEGER PRIMARY KEY \
     AUTOINCREMENT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL, \
     claimed_at TEXT, done_at TEXT, error TEXT)";

/// The index the claim statement reads: the open rows, in claim order.
///
/// `(claimed_at, done_at, id)` and not `(done_at, …)`: every read here is "unfinished
/// rows whose lease expired", so the two nullable columns lead and `id` — the claim
/// order — closes it.
pub const INDEX: &str = "CREATE INDEX IF NOT EXISTS requests_open ON requests \
     (claimed_at, done_at, id)";

/// The kinds a caller may push. A closed set, because the agent dispatches on it:
/// a row whose kind nothing answers is a request that waits forever, and the store
/// is the only place that can refuse it before it is stored.
///
/// This is a protocol, not a vocabulary: it names six operations of the app, not
/// anything about the news. Adding a kind is adding an operation to the agent.
///
/// `settings_set` is one settings key, one value, written by the agent because the
/// window may not write the archive. Its payload is
/// `{"key": "search_engine", "value": "<template>"}`, and the agent checks the value
/// with the same gate the search tool uses before it stores it.
///
/// `brief` is the newest and the only one a *reader* triggers without asking a question:
/// a page wants the model's prepared summary of what it is showing, and the window cannot
/// write the archive itself. Its payload is
/// `{"scope": "country", "key": "DEU", "hours": 72, "why": "<one line>"}` — the scope is
/// one of [`crate::briefings::SCOPES`], the key is that scope's address (`""` for the
/// window) and `why` is the sentence the agent puts in its log. The row the run writes is
/// [`crate::briefings::Briefing`].
pub const KINDS: [&str; 6] = [
    "research",
    "monitor_run",
    "topic_add",
    "enrich",
    "settings_set",
    "brief",
];

/// Largest payload the queue stores. A request is a click's worth of context — a
/// topic name, a monitor id, a sentence — and 64 KiB is far more than any of them;
/// the cap is what stops a bug on the writing side from filling `data/` with one row.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Largest failure note a finished row carries. A stack trace is not a reason; the
/// message a reader needs is the one that fits in a row on screen.
pub const MAX_ERROR_CHARS: usize = 2_000;

/// The sentence a request gets when its payload carries a credential.
///
/// One string, exported, because the window refuses the same payloads and its page has to
/// show the same words: a reader who sees two different sentences for one rule will assume
/// they are two rules. It names the *fix* and never the value — the value is what must not
/// be repeated, and this sentence goes on screen, into a scrollback and into a screenshot.
pub const CREDENTIAL_REFUSAL: &str =
    "this request carries a credential; ATLAS will not store it — remove it from your \
     question and try again";

/// One request, as a claimer receives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    pub created_at: String,
}

/// The queue, over one connection.
///
/// One connection behind one lock is the "one writer" the rest of this crate keeps
/// to: inside this process a claim cannot race another claim, and between
/// processes WAL plus the 5 s busy timeout do the same job by making the write lock
/// a wait instead of an error.
pub struct Requests {
    conn: Mutex<Connection>,
    path: PathBuf,
}

/// Claim one batch: lease the open rows whose lease expired, oldest first.
///
/// `UPDATE … RETURNING` in one statement is the whole concurrency argument. The
/// sub-select is evaluated inside the same write transaction as the `SET`, so the
/// rows it returns are the rows this claimer just took; a second claimer either ran
/// first (and those ids are no longer `claimed_at IS NULL`) or runs after and takes
/// the next ones. No `BEGIN IMMEDIATE`, no read-then-write window to lose.
const CLAIM_SQL: &str = "UPDATE requests SET claimed_at = ?1 \
     WHERE id IN (SELECT id FROM requests WHERE done_at IS NULL \
     AND (claimed_at IS NULL OR claimed_at < ?2) ORDER BY id LIMIT ?3) \
     RETURNING id, kind, payload, created_at";

const PUSH_SQL: &str =
    "INSERT INTO requests (kind, payload, created_at) VALUES (?1, ?2, ?3) RETURNING id";

const FINISH_SQL: &str = "UPDATE requests SET done_at = ?2, error = ?3 WHERE id = ?1";

const DEPTH_SQL: &str = "SELECT count(*) FROM requests WHERE done_at IS NULL";

impl Requests {
    /// Open (creating if needed) the queue at `path`.
    ///
    /// A missing parent directory is created private, and the file itself is made
    /// 0600 from the first byte — the same two rules the archive follows, for the
    /// same reason: a payload says what its reader was looking at.
    pub fn open(path: impl AsRef<Path>) -> Result<Requests> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() {
            return Err(StoreError::new("request queue path is empty"));
        }
        // Only a directory we create is made owner-only: a caller that points this
        // at a directory it already owns (the OS temp dir in a test, `data/` in the
        // app) has decided that directory's mode already.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                perms::ensure_dir(parent);
            }
        }
        if !path.exists() {
            let _ = perms::create_options().open(&path);
        }
        let conn = Connection::open(&path).map_err(StoreError::from)?;
        let queue = Requests {
            conn: Mutex::new(conn),
            path,
        };
        queue.apply_pragmas()?;
        queue.create_schema()?;
        Ok(queue)
    }

    /// The queue's posture: WAL so a reader never blocks the claimer, `NORMAL`
    /// sync so a request survives a crash without paying for a disk flush per
    /// push, and the crate's 5 s busy timeout so two processes take turns.
    fn apply_pragmas(&self) -> Result<()> {
        let conn = self.lock();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(StoreError::from)?;
        if mode.to_ascii_lowercase() != "wal" {
            return Err(StoreError::new(format!(
                "request queue stayed in '{}' - it must be WAL",
                mode
            )));
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")
            .map_err(StoreError::from)?;
        conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(StoreError::from)?;
        Ok(())
    }

    fn create_schema(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch(SCHEMA).map_err(StoreError::from)?;
        conn.execute_batch(INDEX).map_err(StoreError::from)?;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The file this queue lives in — for a log line, never for the archive.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Store one request and answer its id.
    ///
    /// The four refusals are the whole filter: a kind nothing dispatches on, an
    /// empty payload, a payload past the cap, and a payload carrying a credential.
    /// What the rest of the payload *says* is not read here — `{"query": "…"}` and
    /// `{"monitor_id": 3}` are the same thing to this file, and only the agent knows
    /// which one it is holding.
    ///
    /// The credential refusal (`atlas-guard`'s redaction, asked once, never
    /// re-implemented here) is a **refusal, not a rewrite**: a row is read by another
    /// process, and a payload silently repaired would send the agent a question the
    /// reader did not write — `[redacted]` inside a topic name is a topic nobody asked
    /// for. The sentence names the fix, because the reader is the only one who can make
    /// it: take the credential out of the request.
    pub fn push(&self, kind: &str, payload: &str) -> Result<i64> {
        if !KINDS.contains(&kind) {
            return Err(StoreError::new(format!(
                "unknown request kind '{kind}' - expected one of {}",
                KINDS.join(", ")
            )));
        }
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(StoreError::new(format!(
                "a request payload is capped at {MAX_PAYLOAD_BYTES} bytes; this one is {}",
                payload.len()
            )));
        }
        // Shape, not meaning: one JSON object. A caller that hands this a sentence
        // has a bug worth catching here, and a store that parsed the object would
        // need to know every field of every kind.
        let shape = payload.trim();
        if shape.is_empty() {
            return Err(StoreError::new("a request needs a payload"));
        }
        if !(shape.starts_with('{') && shape.ends_with('}')) {
            return Err(StoreError::new(
                "a request payload is one JSON object, like {\"query\": \"...\"}",
            ));
        }
        // The credential check comes after the shape checks so that a caller with a
        // malformed payload is told about *that* first, and before the row exists so
        // that nothing has to be undone.
        //
        // `carries_a_credential`, not `redact(...).hits`: `redact`'s count includes the
        // e-mail pass, and a question that names an address ("what happened to
        // analyst@example.com?") is a question, not a secret. It is not `credential_in`
        // either: that asks "is this value a credential", which is the right question for a
        // stored setting (`meta::set_meta`) and the wrong one here — a reader who pastes a
        // key next to their own words pastes it into a *string*, and this door must refuse
        // it (`a_payload_carrying_an_unnamed_token_is_refused_too`). What both questions
        // share, through guard, is that a location's path is a citation: a payload naming a
        // Google News story over its `CBMi…` path id is research, not a leak.
        if atlas_guard::redact::carries_a_credential(payload) {
            return Err(StoreError::new(CREDENTIAL_REFUSAL));
        }
        let conn = self.lock();
        conn.query_row(PUSH_SQL, rusqlite::params![kind, payload, now_iso()], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(StoreError::from)
    }

    /// Claim up to `limit` requests that are open and not leased right now.
    ///
    /// `now` is the stamp written to `claimed_at` (the caller owns the clock, as
    /// everywhere in this crate), and a row is claimable again when its
    /// `claimed_at` is older than `stale_after_secs` — the lease that lets a
    /// crashed child's request be picked up by the next one.
    pub fn claim_due(
        &self,
        now: &str,
        stale_after_secs: i64,
        limit: usize,
    ) -> Result<Vec<Request>> {
        let stale_before = iso_from_epoch(parse_epoch(now) - stale_after_secs.max(0));
        // `LIMIT -1` in SQLite means *no* limit, so a `usize` past `i64::MAX` must not
        // be allowed to wrap into one: `try_from` and a clamp keep the sentence "at
        // most `limit`" true for every input.
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.lock();
        let mut statement = conn.prepare(CLAIM_SQL).map_err(StoreError::from)?;
        let rows = statement
            .query_map(
                rusqlite::params![now, stale_before, limit],
                |row| {
                    Ok(Request {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        payload: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .map_err(StoreError::from)?;
        let mut claimed = Vec::with_capacity(limit.clamp(0, 64) as usize);
        for row in rows {
            claimed.push(row.map_err(StoreError::from)?);
        }
        Ok(claimed)
    }

    /// Close a request: stamp `done_at`, keep the failure note if there was one.
    ///
    /// The note is capped and stored as text; a finished row is never claimed again
    /// whether it succeeded or not, because a request that failed is a fact — the
    /// next pass is a new push, not a second run of the same row.
    pub fn finish(&self, id: i64, error: Option<&str>) -> Result<()> {
        let note = error.map(|text| {
            if text.chars().count() <= MAX_ERROR_CHARS {
                return text.to_string();
            }
            text.chars().take(MAX_ERROR_CHARS).collect::<String>()
        });
        let conn = self.lock();
        let changed = conn
            .execute(FINISH_SQL, rusqlite::params![id, now_iso(), note])
            .map_err(StoreError::from)?;
        if changed == 0 {
            return Err(StoreError::new(format!("no request {id} to finish")));
        }
        Ok(())
    }

    /// How many requests are still open — pushed and not finished, however often
    /// they were claimed. This is what the status row shows as "waiting".
    pub fn depth(&self) -> Result<i64> {
        let conn = self.lock();
        conn.query_row(DEPTH_SQL, [], |row| row.get::<_, i64>(0))
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A directory of this test's own, under the OS temp dir — never `/tmp` written
    /// out, so this runs on Windows too.
    fn temp_dir(label: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-requests-{}-{}-{}",
            label,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn queue(label: &str) -> Requests {
        Requests::open(temp_dir(label).join("requests.db")).expect("open the queue")
    }

    /// Every claim this test makes starts from a fixed clock: a lease test decides
    /// what "stale" means, so the stamps must not come from the wall.
    const T0: &str = "2026-11-22T10:00:00Z";

    #[test]
    fn the_file_is_created_where_it_was_asked_for() {
        let dir = temp_dir("created");
        let path = dir.join("nested").join("requests.db");
        assert!(!path.exists(), "the fixture starts without a file");
        let queue = Requests::open(&path).expect("open");
        assert!(path.is_file(), "the queue file was not created");
        assert_eq!(queue.path(), path.as_path());
        // The parent was ours to create, so it is ours to make private.
        assert!(path.parent().expect("parent").is_dir());
    }

    #[test]
    fn the_schema_is_the_one_the_callers_were_told_about() {
        let queue = queue("schema");
        let conn = Connection::open(queue.path()).expect("read the schema back");
        let table: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='requests'",
                [],
                |row| row.get(0),
            )
            .expect("the table");
        for column in [
            "id INTEGER PRIMARY KEY AUTOINCREMENT",
            "kind TEXT NOT NULL",
            "payload TEXT NOT NULL",
            "created_at TEXT NOT NULL",
            "claimed_at TEXT",
            "done_at TEXT",
            "error TEXT",
        ] {
            assert!(table.contains(column), "the table lost {column:?}: {table}");
        }
        let index: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='requests_open'",
                [],
                |row| row.get(0),
            )
            .expect("the index");
        assert_eq!(index, 1, "the claim index is missing");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal", "the queue is not in WAL");
    }

    #[test]
    fn a_pushed_request_comes_back_whole() {
        let queue = queue("push");
        // Whitespace and newlines are the caller's, and the store keeps them: the
        // payload is text, not something to normalise on the way through.
        let payload = "{\n  \"query\": \"chip export controls\",\n  \"hours\": 72\n}";
        let id = queue.push("research", payload).expect("push");
        assert_eq!(queue.depth().expect("depth"), 1);
        let claimed = queue.claim_due(T0, 60, 10).expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, id);
        assert_eq!(claimed[0].kind, "research");
        assert_eq!(claimed[0].payload, payload);
        assert_eq!(claimed[0].created_at.len(), "2026-11-22T10:00:00Z".len());
        assert!(claimed[0].created_at.ends_with('Z'), "not an ISO stamp");
    }

    /// The set itself, pinned. Adding a kind adds an operation to the agent, a branch to
    /// the serve loop and a name to the window's own copy of this list
    /// (`atlas-desktop/src/requests.rs`), so the number only moves when all three do.
    #[test]
    fn the_closed_set_is_six_operations() {
        assert_eq!(KINDS.len(), 6, "the closed set changed shape: {KINDS:?}");
        assert_eq!(
            KINDS,
            [
                "research",
                "monitor_run",
                "topic_add",
                "enrich",
                "settings_set",
                "brief"
            ],
            "a kind was renamed or reordered - every dispatcher reads these names"
        );
        // `brief` is the one a reader triggers without asking a question, and its payload
        // is the briefing slot the run fills.
        assert!(KINDS.contains(&"brief"));
    }

    #[test]
    fn an_unknown_kind_is_refused_and_not_stored() {
        let queue = queue("kind");
        let error = queue
            .push("gossip", "{\"query\": \"x\"}")
            .expect_err("a kind nothing dispatches on must not be stored");
        assert!(
            error.message().contains("unknown request kind"),
            "unexpected refusal: {}",
            error.message()
        );
        assert_eq!(queue.depth().expect("depth"), 0, "the refused row was stored");
        // And each kind of the set does go in, so the refusal is about the set and
        // not about pushing.
        for kind in KINDS {
            queue.push(kind, "{}").expect("a known kind");
        }
        assert_eq!(queue.depth().expect("depth"), KINDS.len() as i64);
    }

    #[test]
    fn a_megabyte_payload_is_refused() {
        let queue = queue("cap");
        let payload = format!("{{\"query\": \"{}\"}}", "a".repeat(1_000_000));
        let error = queue
            .push("research", &payload)
            .expect_err("a 1 MB payload must not be stored");
        assert!(
            error.message().contains("capped at"),
            "unexpected refusal: {}",
            error.message()
        );
        assert_eq!(queue.depth().expect("depth"), 0);
        // The cap is on the payload, so a payload of exactly the cap still goes in.
        let body = "a".repeat(MAX_PAYLOAD_BYTES - 2);
        assert_eq!(body.len() + 2, MAX_PAYLOAD_BYTES);
        queue
            .push("research", &format!("{{{body}}}"))
            .expect("the cap itself is allowed");
    }

    #[test]
    fn a_payload_that_is_not_one_object_is_refused() {
        let queue = queue("shape");
        for payload in ["", "   ", "chip export", "[\"research\"]", "{\"query\": "] {
            assert!(
                queue.push("research", payload).is_err(),
                "{payload:?} was stored as a request"
            );
        }
        assert_eq!(queue.depth().expect("depth"), 0);
    }

    #[test]
    fn a_stale_claim_is_claimable_again() {
        let queue = queue("stale");
        let id = queue.push("enrich", "{\"article_id\": 7}").expect("push");
        let first = queue.claim_due(T0, 60, 10).expect("first claim");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, id);
        // Inside the lease: still the first claimer's row.
        assert!(
            queue
                .claim_due("2026-11-22T10:00:30Z", 60, 10)
                .expect("early claim")
                .is_empty(),
            "a leased row was handed out twice"
        );
        // Past the lease: the row a crashed child was holding comes back.
        let again = queue.claim_due("2026-11-22T10:01:01Z", 60, 10).expect("stale claim");
        assert_eq!(again.len(), 1, "a stale claim never came back");
        assert_eq!(again[0].id, id);
        assert_eq!(again[0].payload, "{\"article_id\": 7}");
    }

    #[test]
    fn a_finished_request_never_returns() {
        let queue = queue("finished");
        let id = queue.push("monitor_run", "{\"monitor_id\": 3}").expect("push");
        let claimed = queue.claim_due(T0, 60, 10).expect("claim");
        assert_eq!(claimed.len(), 1);
        queue.finish(id, Some("the page did not load")).expect("finish");
        assert_eq!(queue.depth().expect("depth"), 0);
        assert!(
            queue
                .claim_due("2030-01-01T00:00:00Z", 0, 10)
                .expect("claim after finishing")
                .is_empty(),
            "a finished row was claimed again"
        );
        // The note is kept, and a row that could not be finished is not a row.
        let conn = Connection::open(queue.path()).expect("read the row back");
        let (done, note): (String, String) = conn
            .query_row("SELECT done_at, error FROM requests WHERE id = ?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("the finished row");
        assert!(done.ends_with('Z'), "done_at is not an ISO stamp: {done}");
        assert_eq!(note, "the page did not load");
        assert!(queue.finish(9_999, None).is_err(), "finishing a ghost row passed");
    }

    #[test]
    fn two_claimers_never_receive_the_same_row() {
        // 200 rows, two handles on the same file — two connections, not one behind
        // one lock, which is the case that matters: the claim has to be atomic in
        // SQLite, not merely serialised by a `Mutex`.
        let queue = queue("two-claimers");
        for index in 0..200 {
            queue
                .push("research", &format!("{{\"query\": \"story {index}\"}}"))
                .expect("push");
        }
        assert_eq!(queue.depth().expect("depth"), 200);

        let path = queue.path().to_path_buf();
        let claims = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = path.clone();
                    scope.spawn(move || {
                        let queue = Requests::open(&path).expect("a second claimer");
                        let mut mine: Vec<i64> = Vec::new();
                        loop {
                            let batch = queue.claim_due(T0, 60, 7).expect("claim");
                            if batch.is_empty() {
                                return mine;
                            }
                            mine.extend(batch.into_iter().map(|request| request.id));
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("claimer"))
                .collect::<Vec<Vec<i64>>>()
        });

        let claimed: usize = claims.iter().map(|ids| ids.len()).sum();
        assert_eq!(claimed, 200, "the claimers between them took {claimed} of 200 rows");
        let unique: HashSet<i64> = claims.iter().flatten().copied().collect();
        assert_eq!(unique.len(), 200, "a row was handed to both claimers");
        assert_eq!(queue.depth().expect("depth"), 200, "claiming finished a row");
    }

    #[test]
    fn a_tight_lease_cannot_lose_a_row_either() {
        // The same race with `stale_after_secs = 0`: every claim looks expired to
        // the next one, so this is the worst case for a duplicate. Two claimers,
        // 50 rows, and the union still has to be exactly the 50 ids.
        let queue = queue("tight-lease");
        for index in 0..50 {
            queue
                .push("topic_add", &format!("{{\"topic\": \"t{index}\"}}"))
                .expect("push");
        }
        let path = queue.path().to_path_buf();
        let claims = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = path.clone();
                    scope.spawn(move || {
                        let queue = Requests::open(&path).expect("a claimer");
                        let mut mine: Vec<i64> = Vec::new();
                        // Bounded, because with a zero lease a row is legitimately
                        // claimable again: the loop stops when a batch is empty.
                        loop {
                            let batch = queue.claim_due(T0, 0, 4).expect("claim");
                            if batch.is_empty() {
                                return mine;
                            }
                            mine.extend(batch.into_iter().map(|request| request.id));
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("claimer"))
                .collect::<Vec<Vec<i64>>>()
        });
        let unique: HashSet<i64> = claims.iter().flatten().copied().collect();
        assert_eq!(unique.len(), 50, "a row vanished or was duplicated");
    }
}

/// The refusal that belongs to the push door, pinned where the decision is made.
#[cfg(test)]
mod credential_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn queue(label: &str) -> Requests {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "atlas-requests-credential-{}-{}-{}",
            label,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Requests::open(dir.join("requests.db")).expect("open the queue")
    }

    #[test]
    fn a_payload_carrying_a_named_credential_is_refused_and_no_row_exists() {
        let queue = queue("named");
        let payload =
            "{\"query\":\"look up https://api.example/v1?q=x&api_key=sk-live-abcdefghijklmnop\"}";
        let refused = queue.push("research", payload).expect_err("refused");
        assert_eq!(refused.message(), CREDENTIAL_REFUSAL);
        assert!(
            !refused.message().contains("sk-live") && !refused.message().contains("abcdefghij"),
            "the refusal repeated the credential: {}",
            refused.message()
        );
        assert!(
            queue.claim_due("2026-11-22T10:00:00Z", 60, 10).expect("claim").is_empty(),
            "a refused request left a row behind"
        );
    }

    #[test]
    fn a_payload_carrying_an_unnamed_token_is_refused_too() {
        // The shape rule, not the vocabulary: this value has no credential word in it.
        let queue = queue("shape");
        let token = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq";
        let payload = format!("{{\"query\":\"look up {token}\"}}");
        let refused = queue.push("research", &payload).expect_err("refused");
        assert_eq!(refused.message(), CREDENTIAL_REFUSAL);
        assert!(!refused.message().contains(token));
        assert!(queue
            .claim_due("2026-11-22T10:00:00Z", 60, 10)
            .expect("claim")
            .is_empty());
    }

    #[test]
    fn an_ordinary_request_is_still_queued_word_for_word() {
        // The refusal must not touch the reader's own words: a question with a citation in
        // it, a slug-bearing URL and a long word all pass unchanged.
        let queue = queue("ordinary");
        for payload in [
            "{\"query\":\"what is happening with chips?\"}",
            "{\"query\":\"see https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/\"}",
            "{\"query\":\"doi:10.1038/s41586-021-03819-2 and 3f2504e0-4f89-11d3-9a0c-0305e82c3301\"}",
            // A Google News citation: the path is a 235-byte base64url id, which satisfies
            // every class test of the token shape. Measured before the position test knew
            // about paths: this payload was refused — a reader pasting a story they were
            // reading was told they had pasted a credential.
            "{\"query\":\"tell me more about https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJMXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21VVlc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5\"}",
            // An address is personal data, not a credential: guard's e-mail pass marks it in
            // output, and this door must not refuse a question about one.
            "{\"query\":\"what happened to analyst@example.com?\"}",
        ] {
            let id = queue
                .push("research", payload)
                .unwrap_or_else(|error| panic!("{} refused: {}", payload, error.message()));
            assert!(id > 0);
        }
        let claimed = queue.claim_due("2026-11-22T10:00:00Z", 60, 10).expect("claim");
        assert_eq!(claimed.len(), 5, "an ordinary request was dropped");
        assert_eq!(claimed[2].payload, "{\"query\":\"doi:10.1038/s41586-021-03819-2 and 3f2504e0-4f89-11d3-9a0c-0305e82c3301\"}");
    }

    #[test]
    fn the_shape_checks_still_come_first() {
        // A payload that is both malformed and credential-bearing is told about the shape:
        // the reader's bug is the earlier fact, and the sentence is about the payload.
        let queue = queue("order");
        let refused = queue
            .push("research", "not an object with api_key=sk-live-abcdefghijklmnop")
            .expect_err("refused");
        assert!(
            refused.message().contains("one JSON object"),
            "{}",
            refused.message()
        );
    }
}
