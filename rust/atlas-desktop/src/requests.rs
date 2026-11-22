//! The window's **one writable file**: a request queue at `data/requests.db`.
//!
//! # Why this exists
//!
//! The shipped window cannot ingest — collection, monitoring and research belong to the
//! supervised agent (`atlas-launcher` starts it, `atlas-ingest` gates what it may do). So a
//! click in the window that means *go and look* cannot be a read, and it must not be a
//! write to the archive either: the archive is the app's evidence, opened
//! `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only` (`src/data.rs`). What is left is a
//! **request**: one row in one small queue that says what the reader wants done, which the
//! agent claims and answers. The queue is an intent, not an action — nothing in this
//! window runs because of a row here.
//!
//! # The contract, shared with `atlas_store::requests`
//!
//! The schema below is the store's, word for word — table, index and kinds — and the store
//! is `rust/atlas-store/src/requests.rs`. This crate links no workspace crate (it is the
//! shipped binary's face, not part of the pipeline), so the contract is a copy, and a copy is
//! only honest if it is checked:
//! [`tests::the_queue_table_and_index_are_the_stores_word_for_word`] reads the store's own
//! constants out of that file and compares them with the strings this module writes, so a
//! column renamed on either side — or a fifth kind added on one — fails in this crate's tests.
//! `tests/no_spawn_no_socket.rs` checks the other half: that this is the window's *only* write.
//! If the store changes the schema, this file changes with it.
//!
//! # The rules this module keeps
//!
//! * **One connection per call**, opened and dropped inside [`push`]: the window never
//!   holds the queue open, so the agent is never waiting on a window that is merely open
//!   and idle.
//! * `SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_CREATE`, a 5 s busy timeout (the agent may be
//!   writing at the same moment; a click must not fail because of a race it cannot see),
//!   and WAL, so a reader in the agent and this writer do not lock each other out.
//! * **Kind and payload are validated before a row exists.** A queue is an interface, not
//!   a bucket: an unknown kind or a broken payload would be a row the agent has to guess
//!   about, so it is refused with a sentence instead.
//! * **No panics.** Every failure is a `String` a page can print: a path that cannot be
//!   created, a locked database, a payload that is not JSON. A window that panicked on a
//!   full disk would lose the reader's place for a button they did not need to press.
//!
//! # What it never does
//!
//! It does not write the archive, `data/config.json`, a log, or a cache. It names one file
//! ([`queue_path`]) and one table, and `tests/no_spawn_no_socket.rs` pins both: the
//! window's write surface is this module, and this module has one target.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// The kinds the supervised agent answers. A request of any other kind is refused before a
/// row exists: an interface, not a bucket.
///
/// `research` — look into a topic or a question further than the archive already has;
/// `monitor_run` — run a monitor now instead of on its cadence; `topic_add` — add a topic
/// or a monitor rule from the reader's own words; `enrich` — go and enrich what the graph
/// has (the agent's own richest, slowest path); `settings_set` — write one setting the
/// reader changed; `brief` — write the model's prepared summary of one scope
/// (`views::brief`, `{"scope","key","hours","why"}`), because the window cannot write the
/// archive and a page that has nothing to show must be able to ask.
pub const KINDS: [&str; 6] = [
    "research",
    "monitor_run",
    "topic_add",
    "enrich",
    "settings_set",
    "brief",
];

/// The table, exactly as `atlas_store::requests` creates it.
///
/// Seven columns: the autoincrement id, the kind, the payload (JSON, as text), the stamp
/// the window wrote, and the three the *agent* owns — claimed, done, and the error a
/// failed request carries. `created_at` is the only one this window ever fills.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS requests (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL, claimed_at TEXT, done_at TEXT, error TEXT)";

/// The index the *agent* claims by, created with the table for the same reason the table is.
///
/// The store's own definition, word for word: `(claimed_at, done_at, id)` because its claim
/// statement reads the unfinished rows whose lease expired, in claim order. The agent would
/// create it when it first opens the queue, so this is not required for correctness — it is
/// here so that a queue created by this window and a queue created by the store are the same
/// file, and so that [`the_queue_table_and_index_are_the_stores_word_for_word`] has both
/// halves to compare.
pub const INDEX: &str = "CREATE INDEX IF NOT EXISTS requests_open ON requests (claimed_at, done_at, id)";

/// The reader's decision on the license screen: one row per accepted document version.
///
/// It lives in the same file as the queue, deliberately. The window has exactly one writable
/// file (`tests/disclosure.rs` pins that), and a second database for one string would be a
/// second place to open, lock and get wrong. The two tables do not know about each other: the
/// queue is an inbox for the supervised agent, this is four columns of local state.
pub const ACCEPTANCE: &str = "CREATE TABLE IF NOT EXISTS acceptance (what TEXT PRIMARY KEY, at TEXT NOT NULL)";

/// The largest payload the queue takes: 64 KB.
///
/// A payload is a few keys — a topic, an id, a reason — so this is not a limit a reader can
/// reach by typing. It is a limit on what a *view* can hand here by mistake (a whole
/// article, a model's answer), because a queue row is read by another process and a
/// multi-megabyte row is a message nobody agreed to send.
pub const MAX_PAYLOAD: usize = 64 * 1024;

/// The queue's file: `<root>/data/requests.db`, beside the archive and the config.
pub fn queue_path() -> PathBuf {
    crate::assets::root().join("data").join("requests.db")
}

/// Queue one request, and answer its id.
///
/// `kind` must be one of [`KINDS`]; `payload` must be JSON and at most [`MAX_PAYLOAD`]
/// bytes. The error is a sentence for the status line, never a panic.
pub fn push(kind: &str, payload: &str) -> Result<i64, String> {
    push_to(&queue_path(), kind, payload)
}

/// The same, against a named file: what the tests use, so they never touch `data/`.
pub fn push_to(file: &Path, kind: &str, payload: &str) -> Result<i64, String> {
    check(kind, payload)?;
    let connection = open(file)?;
    connection
        .execute(
            "INSERT INTO requests (kind, payload, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![kind, payload, stamp()],
        )
        .map_err(|problem| {
            format!("the window could not write the request queue: {}", folded(&problem.to_string()))
        })?;
    Ok(connection.last_insert_rowid())
}

// ------------------------------------------------------------------------ the payloads
//
// The four requests, built here rather than in the views: the payload is the *wire
// format* between this window and the agent, so the shape belongs beside the schema that
// carries it. Views pass a name or an id and get JSON back; a quote, a newline or a `%`
// in a topic's name is escaped by `serde_json` and can never end the string early — which
// is the same reason a page never formats JSON by hand.

/// `research`: look into a topic as far as `max_steps` steps allow.
pub fn research_payload(topic: &str, max_steps: u32) -> String {
    serde_json::json!({ "topic": topic, "max_steps": max_steps }).to_string()
}

/// `monitor_run`: run one monitor now. `reason` is for the agent's own record.
pub fn monitor_run_payload(id: i64, reason: &str) -> String {
    serde_json::json!({ "id": id, "reason": reason }).to_string()
}

/// `topic_add`: add what the reader typed. One field, used as both name and query: the
/// reader's sentence *is* the question the monitor asks, and inventing a second name for
/// it would be the window writing words nobody said.
pub fn topic_add_payload(name: &str, query: &str) -> String {
    serde_json::json!({ "name": name, "query": query }).to_string()
}

/// `settings_set`: the settings the reader changed, and why, in one sentence.
///
/// The shared shape, agreed across the window, the store and the agent:
/// `{"settings": {"model_context": 65536}, "why": "…"}`. A *map* and not one key, because a
/// press can be about one field and the reader may have several to send; and `why` because a
/// request is an intent another process acts on, and the agent records what it was told.
///
/// Escaped by `serde_json` throughout, so a model path with a space, an `&` in a URL or a
/// quote in a template travels as one string and cannot end the payload early.
pub fn settings_payload(settings: &serde_json::Map<String, serde_json::Value>, why: &str) -> String {
    serde_json::json!({ "settings": serde_json::Value::Object(settings.clone()), "why": why })
        .to_string()
}

/// One setting, in the same shape: `settings_of("search_engine", json!(template))`.
pub fn settings_of(key: &str, value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut settings = serde_json::Map::new();
    settings.insert(key.to_string(), value);
    settings
}

/// `import_model`: the file or link the reader pasted, in the same envelope — `atlas-setup`
/// copies or downloads it, the window only asks.
pub fn import_payload(source: &str, why: &str) -> String {
    serde_json::json!({ "import_model": source.trim(), "why": why }).to_string()
}

// ------------------------------------------------------------------------ the acceptance
//
// Four small functions, and they are the whole of "remember that the reader read the terms".
// The key is the document's version (`views::license::VERSION`), so a new version is a new
// question: an old row can never answer for a text the reader has not seen.

/// Has this exact document version been accepted? `false` on any failure — an unreadable
/// record is not a signature.
pub fn accepted(what: &str) -> bool {
    accepted_in(&queue_path(), what)
}

/// The same, against a named file: what a window over a fixture asks, so a test never
/// touches `data/`.
pub fn accepted_at(file: &Path, what: &str) -> bool {
    accepted_in(file, what)
}

/// The same, against a named file, so a caller inside this module keeps its own name.
pub fn accepted_in(file: &Path, what: &str) -> bool {
    let Ok(connection) = open(file) else {
        return false;
    };
    connection
        .query_row(
            "SELECT 1 FROM acceptance WHERE what = ?1",
            [what],
            |_| Ok(()),
        )
        .is_ok()
}

/// Record the reader's acceptance of one document version. Answering `Ok` means the row is on
/// disk, not that it was attempted: the screen closes on `Ok` and nowhere else.
pub fn accept(what: &str) -> Result<(), String> {
    accept_in(&queue_path(), what)
}

/// The same, against the window's own queue file: what the terms screen calls on Accept.
pub fn accept_at(file: &Path, what: &str) -> Result<(), String> {
    accept_in(file, what)
}

/// The same, against a named file.
pub fn accept_in(file: &Path, what: &str) -> Result<(), String> {
    if what.trim().is_empty() {
        return Err("the acceptance record needs to know what was accepted".to_string());
    }
    let connection = open(file)?;
    connection
        // `INSERT OR REPLACE`, not `INSERT … ON CONFLICT … DO UPDATE`: the same idempotent
        // record, spelled without the word `UPDATE`, because the window's write rule
        // (`tests/no_spawn_no_socket.rs`) forbids an update on any table — the queue's rows
        // belong to the agent once written, and a rule with an exception for a second table
        // is a rule nobody can check. Accepting the same version twice keeps one row.
        .execute(
            "INSERT OR REPLACE INTO acceptance (what, at) VALUES (?1, ?2)",
            rusqlite::params![what, stamp()],
        )
        .map_err(|problem| format!("the window could not record your acceptance: {problem}"))?;
    Ok(())
}

/// The sentence a request gets when its payload carries a credential.
///
/// Word for word `atlas_store::requests::CREDENTIAL_REFUSAL`: the agent refuses the same
/// payloads at the other door of the same table, and a reader who sees two sentences for one
/// rule will assume they are two rules. It is repeated here rather than imported because this
/// crate links no workspace crate at all — `tests/fresh_machine.rs` pins the four dependencies
/// and `tests/disclosure.rs` pins their kind — and that property is worth more than the
/// duplication, which a test in `tests/config_secrets.rs` keeps honest by comparing the two
/// strings.
pub const CREDENTIAL_REFUSAL: &str =
    "this request carries a credential; ATLAS will not store it — remove it from your \
     question and try again";

/// The shortest run that is a token and not a word: 40 characters.
///
/// The same number `atlas_guard::redact` uses (`TOKEN_MIN_CHARS`), measured there against the
/// shapes the feeds carry — a UUID is 36, a news slug is lower case, and a 40-byte run of
/// `[A-Za-z0-9_-]` that mixes case *and* has a digit is not a word in any of the six languages
/// this app reads.
pub const TOKEN_MIN_CHARS: usize = 40;

/// Does this payload carry a token-shaped value?
///
/// **Shape, not vocabulary** — that is the only kind of credential rule this app allows, and
/// it is also the only kind this crate can host: the named half (`api_key=`, `sk-live-…`) is
/// `atlas-guard`'s, and the window does not link it. What the window *can* decide alone is
/// whether the reader's text contains a run of 40+ token bytes mixing case and digits, which
/// no sentence in these six languages produces by accident.
///
/// A payload is a value in its entirety — it is the content of a request, not a quotation of
/// someone else — so prose in it needs no "value position" test. A *location* in it does: a
/// URL's path is where the citation lives, and only its query or fragment is a value position.
pub fn carries_a_token(payload: &str) -> bool {
    // The rule is asked once per whitespace-free chunk, because where a token may sit inside
    // one chunk depends on what the chunk is: text the reader typed is a value throughout,
    // while a chunk carrying a scheme is a **location**, and a location's *path* is a citation
    // — only its query and fragment are value positions. That second half is
    // `atlas_guard::redact`'s rule, and the archive is why it exists: measured over the 1439
    // URLs in `data/atlas.db`, treating a path as a value position redacted 231 of them, every
    // Google News `CBMi…` id (a 235-byte base64url span that satisfies the class test below).
    // The same defect sat at this door: a reader pasting the story they were reading was told
    // they had pasted a credential (`a_pasted_citation_is_not_a_credential`).
    payload
        .split(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r')
        .any(|chunk| {
            let gate = match chunk.find("://") {
                // In a location only what follows `?` or `#` is a value.
                Some(_) => chunk.find('?').or_else(|| chunk.find('#')),
                // Not a location: the whole chunk is the reader's own text.
                None => Some(0),
            };
            match gate {
                Some(floor) => chunk_carries_a_token(chunk, floor),
                // A location with no query and no fragment has no value position at all.
                None => false,
            }
        })
}

/// Does `chunk` hold a token-shaped run that starts at or after `floor`?
///
/// Two of each class, not one: `atlas_guard`'s rule learned this from a measurement of the
/// archive's own URLs — a publisher slug that follows a percent-encoded byte
/// (`%9B-usa-fiona-…`) carries exactly one upper-case letter in a long lower-case run and is a
/// citation, not a key. The two rules must agree, or the two doors of one table would refuse
/// different requests.
fn chunk_carries_a_token(chunk: &str, floor: usize) -> bool {
    let (mut run, mut start) = (0usize, 0usize);
    let (mut digit, mut upper, mut lower) = (0usize, 0usize, 0usize);
    for (at, byte) in chunk
        .bytes()
        .chain(std::iter::once(b' '))
        .enumerate()
    {
        let token_byte = byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_';
        if token_byte {
            if run == 0 {
                start = at;
            }
            run += 1;
            if byte.is_ascii_digit() {
                digit += 1;
            } else if byte.is_ascii_uppercase() {
                upper += 1;
            } else if byte.is_ascii_lowercase() {
                lower += 1;
            }
            continue;
        }
        if start >= floor && run >= TOKEN_MIN_CHARS && digit >= 2 && upper >= 2 && lower >= 2 {
            return true;
        }
        run = 0;
        digit = 0;
        upper = 0;
        lower = 0;
    }
    false
}

/// Refuse anything the agent could not answer, before a row exists.
fn check(kind: &str, payload: &str) -> Result<(), String> {
    if !KINDS.contains(&kind) {
        // The kind is named in the sentence: a reader who sees this is looking at a bug in
        // the window, not at the queue, and the name is what makes it reportable.
        return Err(format!("the queue does not take a `{kind}` request"));
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(format!(
            "that request is {} bytes and the queue takes {}",
            payload.len(),
            MAX_PAYLOAD
        ));
    }
    if serde_json::from_str::<serde_json::Value>(payload).is_err() {
        return Err("that request is not JSON, so the agent could not read it".to_string());
    }
    // A refusal, never a rewrite: a row is read by another process, and a payload the window
    // silently repaired would send the agent a question the reader did not write. The shape
    // checks come first, so a malformed payload is reported as malformed.
    if carries_a_token(payload) {
        return Err(CREDENTIAL_REFUSAL.to_string());
    }
    Ok(())
}

/// One short-lived connection: create the file and the table if they are missing, then hand
/// back a connection the caller drops.
/// The driver's own words, folded into something a status line can paint.
///
/// Two things are wrong with a raw `rusqlite` message in a row of the window: it can carry an
/// absolute path (the queue's own file, and with it the reader's home directory), and it has no
/// final full stop. The fold is structural, not a word list: every run that *starts* a path —
/// a `/`, a `~`, or a drive letter followed by a separator, at a word boundary — is dropped up
/// to the next whitespace, the rest is collapsed onto one line, and the sentence gets its full
/// stop. A message that is only a path therefore folds to nothing, and the caller's own clause
/// is the whole sentence, which is still true.
fn folded(problem: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    for (index, word) in problem.split_whitespace().enumerate() {
        let starts_a_path = {
            let bytes = word.as_bytes();
            match bytes.first() {
                Some(b'/') | Some(b'~') => true,
                // `C:\Users\…`: one letter, a colon, a separator. At index 0 too, because a
                // message can begin with the path it is about.
                Some(letter)
                    if letter.is_ascii_alphabetic()
                        && bytes.get(1) == Some(&b':')
                        && matches!(bytes.get(2), Some(b'\\') | Some(b'/')) =>
                {
                    true
                }
                _ => false,
            }
        };
        if starts_a_path {
            continue;
        }
        // A path glued to a word ("in/tmp/x") is not a path this rule can see; dropping the
        // whole word would delete the reader's only clue about *what* failed, so it stays.
        let _ = index;
        words.push(word);
    }
    let mut sentence = words.join(" ");
    if sentence.is_empty() {
        return String::new();
    }
    if !sentence.ends_with('.') {
        sentence.push('.');
    }
    sentence
}

fn open(file: &Path) -> Result<Connection, String> {
    if let Some(parent) = file.parent() {
        // The window may be the first thing to run in a fresh checkout: a queue that
        // cannot create its own directory would be a button that fails on a new machine.
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|problem| {
                format!("the window could not make room for the request queue: {}", folded(&problem.to_string()))
            })?;
        }
    }
    let connection = Connection::open_with_flags(
        file,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|problem| format!("the window could not open the request queue: {}", folded(&problem.to_string())))?;
    // Owner-only, like every other file the app keeps in `data/`. A queue row is the reader's
    // own words (the monitor question they typed) and the sentences the window built for
    // them, and SQLite creates its file 0644 under the usual umask — measured on this
    // machine: `data/requests.db` was `-rw-r--r--` while `data/atlas.db` is `-rw-------`.
    // Repairing on every open also fixes an install that already has the loose file, and it
    // runs before WAL is switched on below so the `-wal` and `-shm` SQLite creates beside it
    // inherit the tight mode instead of the loose one. A failure here is not fatal: the queue
    // still works, and a row of the window is not worth losing over a mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    }
    // Five seconds: the agent may be inserting or claiming at this instant, and the reader
    // pressed a button once. Long enough to outlast a claim, short enough to come back.
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|problem| format!("the window could not wait for the request queue: {}", folded(&problem.to_string())))?;
    // WAL: the agent reads this table while the window writes it, and a rollback journal
    // would make one wait for the other. The mode is a property of the file, so this is a
    // no-op on every call after the first — and it answers with the mode in a row, hence
    // `query_row` rather than `execute_batch`.
    connection
        .query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
        .map_err(|problem| format!("the window could not set the queue's journal: {}", folded(&problem.to_string())))?;
    // …and the two files WAL creates beside it. SQLite makes them with the database's mode, so
    // a fresh queue is covered by the line above — but an install that already has a loose
    // `requests.db-wal` keeps it: measured on this machine, the database was tightened to 0600
    // and its `-wal` was still `-rw-r--r--` from before. The side files hold the same rows
    // (WAL is where a write lands first), so a repair that skips them repairs nothing.
    #[cfg(unix)]
    for side in ["-wal", "-shm"] {
        use std::os::unix::fs::PermissionsExt;
        // An `OsString` suffix, not a formatted string: the queue's name is an `OsStr` on
        // every platform, and a path built by pasting text would be the one place in this
        // crate a path becomes a sentence and comes back (`tests/disclosure.rs`'s rule about
        // `display()` — the strict form of it, which is the one that stays honest).
        let mut beside = file.to_path_buf().into_os_string();
        beside.push(side);
        let _ = std::fs::set_permissions(
            std::path::PathBuf::from(beside),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    connection
        .execute_batch(SCHEMA)
        .map_err(|problem| format!("the window could not prepare the request queue: {}", folded(&problem.to_string())))?;
    connection
        .execute_batch(INDEX)
        .map_err(|problem| format!("the window could not prepare the queue's index: {problem}"))?;
    connection
        .execute_batch(ACCEPTANCE)
        .map_err(|problem| format!("the window could not prepare the acceptance record: {problem}"))?;
    Ok(connection)
}

/// Now, in the archive's stamp format: `YYYY-MM-DDTHH:MM:SSZ`, UTC.
///
/// Written here rather than taken from a crate: this binary has no time dependency, and the
/// one thing it needs is the same shape the archive's `last_hit`/`first_seen` columns use
/// (`views::when` reads it, and its test asserts the shape this produces is the shape it
/// parses).
pub fn stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's `civil_from_days`).
///
/// The same arithmetic as `views::utc_day`, kept local because the alternative is a public
/// helper for two callers in one crate — and because this one has to be testable on a day
/// that is not today.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh queue in a directory of the test's own, removed afterwards.
    fn queue() -> (PathBuf, PathBuf) {
        let name = format!(
            "atlas-desktop-requests-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(name.replace(['(', ')', ' '], "-"));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("data").join("requests.db");
        (dir, file)
    }

    fn rows(file: &Path) -> Vec<(String, String, String)> {
        let connection = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("the queue the test just wrote");
        let mut statement = connection
            .prepare("SELECT kind, payload, created_at FROM requests ORDER BY id")
            .expect("the table exists");
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("a query")
            .map(|row| row.expect("a row"))
            .collect()
    }

    #[test]
    fn a_request_is_one_row_with_the_kind_the_payload_and_a_stamp() {
        let (dir, file) = queue();
        let id = push_to(&file, "research", r#"{"topic":"chips","max_steps":6}"#)
            .expect("the queue takes a research request");
        assert!(id >= 1, "the queue answered id {id}");
        let taken = rows(&file);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, "research");
        assert_eq!(taken[0].1, r#"{"topic":"chips","max_steps":6}"#, "the payload is stored verbatim");
        // The stamp is the archive's shape, and the page's own formatter agrees with it.
        let stamp = &taken[0].2;
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T', "{stamp}");
        for (index, byte) in stamp.bytes().enumerate() {
            assert!(
                index == 4 || index == 7 || index == 10 || index == 13 || index == 16 || index == 19
                    || byte.is_ascii_digit(),
                "{stamp}: byte {index} is {}", byte as char
            );
        }
        // `views::when` reads `YYYY-MM-DDTHH:MM:SSZ`; a stamp of today must come back as a
        // clock time, which is how the reader sees it.
        let shown = crate::views::when(stamp);
        assert!(
            shown.len() == 5 && shown.as_bytes()[2] == b':',
            "the queue's stamp does not parse as a timestamp: {shown:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_four_kinds_are_taken_and_nothing_else_reaches_the_table() {
        let (dir, file) = queue();
        for kind in ["research", "monitor_run", "topic_add", "enrich"] {
            push_to(&file, kind, "{}").unwrap_or_else(|problem| panic!("{kind}: {problem}"));
        }
        assert_eq!(rows(&file).len(), 4, "the four kinds are accepted");
        for refused in ["Research", "re-search", "", "spawn", "research "] {
            let problem = push_to(&file, refused, "{}").expect_err("an unknown kind is refused");
            assert!(
                problem.contains(refused) || refused.is_empty(),
                "the sentence does not name the kind: {problem}"
            );
        }
        assert_eq!(rows(&file).len(), 4, "a refused kind wrote a row");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_payload_that_is_not_json_or_too_long_is_refused_in_a_sentence() {
        let (dir, file) = queue();
        let broken = push_to(&file, "research", "chips").expect_err("not JSON");
        assert!(broken.contains("not JSON"), "{broken}");
        let empty = push_to(&file, "research", "").expect_err("empty is not JSON");
        assert!(empty.contains("not JSON"), "{empty}");
        let big = "x".repeat(MAX_PAYLOAD + 1);
        let huge = push_to(&file, "research", &big).expect_err("too long");
        assert!(huge.contains("65536") && huge.contains(&(MAX_PAYLOAD + 1).to_string()), "{huge}");
        // Exactly the limit is fine: the rule is a limit, not a margin. `{"padding":"…"}` is
        // fourteen bytes of punctuation around the filler.
        let at_limit = format!("{{\"padding\":\"{}\"}}", "x".repeat(MAX_PAYLOAD - 14));
        assert_eq!(at_limit.len(), MAX_PAYLOAD);
        assert!(push_to(&file, "research", &at_limit).is_ok(), "the limit itself is allowed");
        assert_eq!(rows(&file).len(), 1, "the refused payloads wrote rows");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_file_and_the_table_are_created_on_the_first_push_and_the_second_reuses_them() {
        let (dir, file) = queue();
        assert!(!file.exists(), "the test's queue starts absent");
        let first = push_to(&file, "enrich", "{}").expect("the first push creates the queue");
        let second = push_to(&file, "enrich", "{}").expect("the second push reuses it");
        assert_eq!((first, second), (1, 2), "the ids run on");
        assert!(file.exists(), "the queue was not created where it said");
        // The schema is the store's, column for column: the three stamps the *agent* owns
        // exist and are null until it claims the row.
        let connection = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let null_stamps: (Option<String>, Option<String>, Option<String>) = connection
            .query_row("SELECT claimed_at, done_at, error FROM requests LIMIT 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .expect("the three agent columns exist");
        assert_eq!(null_stamps, (None, None, None));
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("the journal mode");
        assert_eq!(mode.to_lowercase(), "wal", "the queue is not in WAL");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_directory_that_cannot_be_written_is_a_sentence_not_a_panic() {
        // A path whose parent is a *file*: `create_dir_all` cannot make it, so the failure
        // this test is about comes from the file system rather than from the flags.
        let (dir, file) = queue();
        std::fs::create_dir_all(&dir).expect("the test's own directory");
        let blocker = dir.join("data");
        std::fs::write(&blocker, b"not a directory").expect("a file where the queue's folder should be");
        let problem = push_to(&file, "research", "{}").expect_err("a directory that is a file");
        assert!(problem.starts_with("the window could not"), "{problem}");
        assert!(problem.contains("request queue") || problem.contains("room"), "{problem}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_payload_is_json_even_when_the_name_it_carries_is_hostile() {
        // The names come from the archive, the archive comes from the web: a topic called
        // `chips","max_steps":999` or one with a newline in it must still be *a string in
        // JSON*, not a second key and not a broken row. Built by `serde_json`, read back by
        // `serde_json`, so the property is the library's and the test only asserts it holds.
        let hostile = "chips\",\"max_steps\":999\n{\"x\":\"y";
        let payload = research_payload(hostile, 6);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
        assert_eq!(parsed["topic"], hostile, "the name did not survive escaping");
        assert_eq!(parsed["max_steps"], 6);
        assert_eq!(parsed.as_object().map(|object| object.len()), Some(2), "{payload}");
        // And the queue takes it: a payload the agent must parse cannot be refused here for
        // a reason the escape should have handled.
        let (dir, file) = queue();
        push_to(&file, "research", &payload).expect("the queue takes an escaped payload");
        let _ = std::fs::remove_dir_all(dir);

        let run: serde_json::Value = serde_json::from_str(&monitor_run_payload(42, "button")).unwrap();
        assert_eq!(run["id"], 42, "the id is a number, not a string");
        assert_eq!(run["reason"], "button");
        let added: serde_json::Value =
            serde_json::from_str(&topic_add_payload("chips", "what is happening with chips?")).unwrap();
        assert_eq!(added["name"], "chips");
        assert_eq!(added["query"], "what is happening with chips?");
        // All three fit under the limit by orders of magnitude: the limit exists for what a
        // view could hand here by mistake, not for what a reader can type.
        for payload in [
            research_payload(&"x".repeat(400), 6),
            monitor_run_payload(i64::MAX, "button"),
            topic_add_payload(&"x".repeat(400), &"y".repeat(400)),
        ] {
            assert!(payload.len() < MAX_PAYLOAD, "{}", payload.len());
        }
    }

    #[test]
    fn the_queue_table_and_index_are_the_stores_word_for_word() {
        // A copy is only honest if it is checked. This crate links no workspace crate, so the
        // store's definition cannot be *called* — it is read, as text, out of the sibling
        // crate (`../atlas-store/src/requests.rs`) and compared with the strings this window
        // writes. Two things this catches that a shape assertion cannot: a column renamed in
        // one place, and a kind added on one side of the wire but not the other.
        //
        // Absent store (a lone checkout of this crate) → the test says so and stops; it never
        // silently passes as if it had compared something.
        let store = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("atlas-store")
            .join("src")
            .join("requests.rs");
        let Ok(source) = std::fs::read_to_string(&store) else {
            eprintln!("skipped: {} is not in this checkout", store.display());
            return;
        };
        let squashed = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let constant = |name: &str| -> String {
            let start = source
                .find(&format!("pub const {name}: &str = \""))
                .unwrap_or_else(|| panic!("the store has no `{name}`"));
            let tail = &source[start..];
            let opened = tail.find('"').expect("an opening quote");
            // The store writes these across lines with `\` continuations, so read to the
            // closing quote that is not escaped — the string itself has none inside.
            let body = &tail[opened + 1..];
            let close = body.find("\";").unwrap_or_else(|| panic!("`{name}` is not a literal"));
            squashed(&body[..close].replace("\\\n", " ").replace('\\', " "))
        };
        assert_eq!(constant("SCHEMA"), squashed(SCHEMA), "the queue's table is not the store's");
        assert_eq!(constant("INDEX"), squashed(INDEX), "the queue's index is not the store's");
        // The store's closed set, read by *word* rather than by `{:?}`: the two lists are the
        // same six strings in the same order, and a store that writes them one per line (as it
        // does) is still the same list. Comparing the Debug of an array would pin the store's
        // formatting as well as its words — `[&str; 6] = ["a", "b"]` on one line passing and the
        // identical list across six lines failing.
        let listed: Vec<String> = {
            let start = source
                .find("pub const KINDS")
                .expect("the store's closed set is called KINDS");
            let tail = &source[start..];
            let opened = tail.find('[').expect("the set's opening bracket");
            let closed = tail[opened..].find("];").expect("the set's closing bracket");
            tail[opened..opened + closed]
                .split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            listed,
            KINDS.iter().map(|kind| kind.to_string()).collect::<Vec<String>>(),
            "the store's kinds are not this window's, word for word"
        );
        assert!(
            source.contains("64 * 1024"),
            "the store's payload cap moved; this window still refuses at {MAX_PAYLOAD}"
        );
    }

    #[test]
    fn the_schema_is_the_stores_and_the_path_is_the_one_beside_the_archive() {
        // The schema string, read by shape rather than by eye: seven columns, the three
        // nullable ones, and the only NOT NULL columns the three the window and the agent
        // must fill.
        for column in [
            "id INTEGER PRIMARY KEY AUTOINCREMENT",
            "kind TEXT NOT NULL",
            "payload TEXT NOT NULL",
            "created_at TEXT NOT NULL",
            "claimed_at TEXT",
            "done_at TEXT",
            "error TEXT",
        ] {
            assert!(SCHEMA.contains(column), "the schema lost `{column}`");
        }
        assert!(SCHEMA.starts_with("CREATE TABLE IF NOT EXISTS requests ("));
        // And it is the file's own name, in the checkout's data directory: one file, named
        // by the same root the archive is read from.
        let path = queue_path();
        assert!(path.ends_with("data/requests.db"), "{}", path.display());
        assert_eq!(
            path.parent().and_then(|data| data.parent()),
            Some(crate::assets::root().as_path()),
            "the queue is not beside the archive"
        );
        // The stamp helper's civil calendar: two known days, so a wrong epoch cannot pass.
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_723), (2024, 1, 1)); // 2024-01-01, counted from 1970-01-01
    }

    // ---- the token-shape rule `check` applies, at its boundary (see `carries_a_token`) ----
    /// 39 bytes: one short of the cap, so it is not a token by this rule.
    const SHORT: &str = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2P";
    /// The same run plus one character: 40 bytes.
    const LONG: &str = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq";

    #[test]
    fn the_cap_is_stated_and_measured() {
        assert_eq!(TOKEN_MIN_CHARS, 40);
        assert_eq!(SHORT.len(), 39);
        assert_eq!(LONG.len(), 40);
        assert!(!carries_a_token(SHORT), "39 bytes must stay");
        assert!(carries_a_token(LONG), "40 bytes must go");
    }

    #[test]
    fn a_citation_a_slug_a_uuid_and_a_sentence_are_not_tokens() {
        // The shapes a reader's own words take, all of them several words long and none of
        // them 40 mixed-case-with-digit bytes in one run.
        for text in [
            "Nvidia rose 3 percent on Tuesday after TSMC reported demand.",
            "https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/",
            "doi:10.1038/s41586-021-03819-2",
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            // the real publisher URL the archive measurement turned up: one upper-case letter,
            // inside a percent-escape, in a lower-case slug
            "https://www.france24.com/en/video/20261120-%F0%9F%A6%9B-usa-fiona-cincinnati-zoo-s-famous-hippo-has-died",
            "what is happening with chips?",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!carries_a_token(text), "{text} was read as a token");
        }
    }

    #[test]
    fn a_token_anywhere_in_a_payload_refuses_the_request() {
        // The payload is a value in its entirety, so there is no position test here — unlike
        // `atlas_guard::redact`, which also scans log lines and prompts where prose is the
        // point.
        let payload = format!("{{\"query\":\"look up {LONG} please\"}}");
        let refused = check("research", &payload).expect_err("refused");
        assert_eq!(refused, CREDENTIAL_REFUSAL);
        assert!(!refused.contains(LONG), "the refusal repeated the token");
        // And the ordinary payload of the same kind still passes.
        assert!(check("research", "{\"query\":\"what is happening with chips?\"}").is_ok());
    }

    #[test]
    fn a_pasted_citation_is_not_a_credential() {
        // The measured defect, at this door: a Google News id is a 235-byte base64url span in
        // the *path* of the URL, so the class test alone read it as a key and the reader was
        // told their citation was a credential (the archive lost 231 of its 1439 URLs to the
        // same rule — `atlas-guard`'s `a_google_news_article_id_in_a_path_stays`). A path is a
        // citation; a query is a value.
        let citation = "{\"query\":\"tell me more about https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJMXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21VVlc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5\"}";
        assert!(
            !carries_a_token(citation),
            "a pasted citation was read as a credential"
        );
        assert!(check("research", citation).is_ok());
        // The query of the same URL is a value position, and a key pasted there is refused.
        let with_key = format!("{{\"query\":\"https://news.google.com/rss?oc=5&key={LONG}\"}}");
        assert!(carries_a_token(&with_key), "a key in a query is a key");
        assert_eq!(
            check("research", &with_key).expect_err("refused"),
            CREDENTIAL_REFUSAL
        );
        // A bare token in the reader's own words is still refused.
        assert!(carries_a_token(&format!("{{\"query\":\"look up {LONG}\"}}")));
    }

    #[test]
    fn an_address_in_a_question_is_a_subject_not_a_secret() {
        // The window links no guard, so it cannot see the e-mail rule at all; the store's door
        // asks guard's credential passes *without* the e-mail pass
        // (`carries_a_credential`). Both doors must queue this.
        let payload = "{\"query\":\"what happened to analyst@example.com?\"}";
        assert!(!carries_a_token(payload));
        assert!(check("research", payload).is_ok());
    }

    #[test]
    fn the_shape_checks_come_first() {
        let refused = check("research", "api_key=sk-live-abcdefghijklmnop").expect_err("refused");
        assert!(
            refused.contains("not JSON"),
            "a malformed payload is reported as malformed: {refused}"
        );
    }
}
