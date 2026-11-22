//! The window cannot write the archive — proved on a temp copy, not on `data/`.
//!
//! ATLAS's face is `atlas-desktop`: it reads `data/atlas.db` through
//! `data::Archive::open` and opens no socket. "Reads" is the whole promise of that
//! module, so it is the one thing worth a test: the connection the window opens must
//! not change a byte, must not put the file into WAL mode, and must not need
//! permission to write.
//!
//! ## How this file reaches the code under test
//!
//! `atlas-desktop` is a library target plus the thin binary that opens the window
//! (`src/lib.rs`), so this test links `atlas_desktop::data` — the module is compiled
//! once, into the library, and this binary calls that compiled copy. It is not a copy of
//! the open function and no rule is re-implemented here: change `data.rs` and this test
//! changes with it. The only SQL this file writes is the fixture's schema and rows.
//!
//! Two consequences of linking instead of including, both deliberate:
//!
//! * `data.rs`'s own `#[cfg(test)]` block belongs to the library's test target and does
//!   not compile into this binary, so the side files it needs are not stubbed here.
//! * `Archive`'s `conn` field is still private with no accessor. This file does **not**
//!   widen the API to reach it — see "What a test outside the module cannot reach".
//!
//! The live `data/atlas.db` is never opened: every fixture below is a copy under the
//! system temp directory.
//!
//! ## What the side files mean
//!
//! The live archive is in WAL mode (the engines set it). Measured: a read-only
//! connection to a WAL database creates `-shm` (32 KiB, the shared-memory index) and
//! an **empty** `-wal` next to it, and the database file itself stays byte-identical
//! with its mtime unmoved. So reading a WAL archive is not "nothing is created"; it
//! is "the WAL stays empty and the archive is untouched", which is what the
//! assertions say. A rollback-journal archive creates nothing at all.
//!
//! ## The one case that is wrong today
//!
//! Measured, and pinned by the ignored test at the end: a WAL archive with no
//! `-shm` yet, in a directory the window cannot write, opens **successfully and
//! reads as an empty archive** — the window would draw "0 stories" over an archive
//! full of stories instead of saying what is wrong. The fix belongs in `data.rs`
//! (refuse a connection that sees no schema in a non-empty file); it cannot be made
//! from here, so the desired behaviour is written as an `#[ignore]`d test that names
//! the fix.
//!
//! ## What a test outside the module cannot reach
//!
//! `Archive`'s `conn` field is private to the `data` module and there is no
//! accessor, so no test outside that module can run `INSERT`/`UPDATE`/`DELETE`/
//! `BEGIN` on the window's connection. That assertion lives *inside* the module —
//! `src/data/tests.rs`, `an_archive_opened_read_only_refuses_a_write`, which executes
//! `DELETE FROM articles` and requires the refusal — and the module's own test file
//! is the only place it can run. This file proves the same property from the outside,
//! at the level a caller can observe: the file cannot change, no journal and no WAL
//! frame can appear, the header's journal mode cannot move, and every query still
//! answers from a copy whose directory is unwritable.
//!
//! `#![allow(dead_code)]` is not a silence, it is the truth about this target: a test
//! binary carries the helpers only some tests call, and this file calls what it needs.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use atlas_desktop::data;
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// fixtures: a temp database with the archive's shape
// ---------------------------------------------------------------------------

/// A fresh directory under the system temp directory, emptied first.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "atlas-readonly-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp directory");
    dir
}

/// The tables and columns the window's queries name, and nothing else: a copy
/// missing one of them would fail the reads for the wrong reason.
fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE articles(
             id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
             published TEXT, summary TEXT, content TEXT, place TEXT, iso TEXT, url TEXT,
             full_text INTEGER, relevant INTEGER, dupe INTEGER, lat REAL, lon REAL);
         CREATE TABLE entities(
             id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER, last_seen TEXT);
         CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL, label TEXT);
         CREATE TABLE article_links(a INTEGER, b INTEGER);
         CREATE TABLE topics(
             id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER,
             hits INTEGER, last_hit TEXT);
         CREATE TABLE monitors(
             id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER,
             cadence_minutes INTEGER, last_run TEXT, hits INTEGER);
         CREATE TABLE alerts(id INTEGER PRIMARY KEY, kind TEXT);",
    )
    .expect("the archive's shape");
}

/// An ISO stamp `hours_ago` hours in the past, in the fixed-width form the archive
/// stores, so the window's `published >= ?1` comparison sees a row inside its window.
fn stamp(hours_ago: i64) -> String {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let at = now - hours_ago * 3_600;
    let (days, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    // Days since the epoch to a civil date (Hinnant's algorithm): no date crate, no
    // time zone, no leap-second table.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        if month <= 2 { year + 1 } else { year },
        month,
        day,
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

/// Create and seed one database file, leaving the writer closed. `journal_mode` is
/// what the engines would have left behind: `delete` for a fresh file, `wal` for the
/// live archive.
fn seed(path: &Path, journal_mode: &str) {
    let conn = Connection::open(path).expect("a writable copy to seed");
    conn.pragma_update(None, "journal_mode", journal_mode)
        .expect("the journal mode the engines use");
    schema(&conn);
    conn.execute("INSERT INTO sources(id, name) VALUES (1, 'wired.com')", [])
        .expect("a source");
    conn.execute(
        "INSERT INTO articles(id, title, publisher, source_id, published, summary, content,
             place, iso, url, full_text, relevant, dupe, lat, lon)
         VALUES (1, 'A story the window can show', 'wired.com', 1, ?1, 'a summary', 'the body',
             'London', 'United Kingdom', 'https://example.test/a', 1, 1, 0, 51.5074, -0.1278)",
        rusqlite::params![stamp(2)],
    )
    .expect("an article inside the window");
    conn.execute(
        "INSERT INTO entities(id, name, type, count, last_seen) VALUES (1, 'Ada', 'person', 3, ?1)",
        rusqlite::params![stamp(1)],
    )
    .expect("an entity");
    conn.execute(
        "INSERT INTO edges(a, b, weight, label) VALUES (1, 1, 0.5, 'works-with')",
        [],
    )
    .expect("an edge");
    conn.execute("INSERT INTO article_links(a, b) VALUES (1, 1)", [])
        .expect("a link");
    conn.execute(
        "INSERT INTO topics(id, name, keywords, active, hits, last_hit) \
         VALUES (1, 'energy', 'grid', 1, 2, ?1)",
        rusqlite::params![stamp(1)],
    )
    .expect("a topic");
    conn.execute(
        "INSERT INTO monitors(id, name, query, active, cadence_minutes, last_run, hits) \
         VALUES (1, 'grid', 'grid', 1, 60, ?1, 0)",
        rusqlite::params![stamp(1)],
    )
    .expect("a monitor");
    conn.execute("INSERT INTO alerts(id, kind) VALUES (1, 'new')", [])
        .expect("an alert");
}

/// Everything about one file that a reader must not disturb.
#[derive(PartialEq, Eq, Debug)]
struct Facts {
    bytes: Vec<u8>,
    len: u64,
    modified: SystemTime,
    /// SQLite header bytes 18 and 19: the file format's write and read version.
    /// `1` is a rollback journal, `2` is WAL. The mode is a property of the *file*,
    /// so it can be read without opening a connection.
    format_version: (u8, u8),
}

fn facts(path: &Path) -> Facts {
    let bytes = std::fs::read(path).expect("the copy is readable");
    let meta = std::fs::metadata(path).expect("the copy exists");
    Facts {
        format_version: (bytes[18], bytes[19]),
        len: meta.len(),
        modified: meta.modified().expect("the copy has an mtime"),
        bytes,
    }
}

/// The names in a directory, sorted — the shapes a journal or a WAL would appear in.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the directory is readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The side files SQLite leaves when it writes: `-journal`, `-wal`, `-shm`.
fn side_files(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|name| name.contains("-journal") || name.contains("-wal") || name.contains("-shm"))
        .collect()
}

/// What the window's queries answered, so a test can prove the reads were real and
/// not silently empty.
#[derive(Debug, PartialEq, Eq)]
struct Read {
    counts: i64,
    articles: usize,
    pins: usize,
    countries: usize,
    entities: usize,
    edges: usize,
    topics: usize,
    monitors: usize,
}

/// Run every query the window runs.
fn read_everything(archive: &data::Archive) -> Read {
    let counts = archive.counts();
    let articles = archive.articles(72, 400, "");
    let pins = archive.pins(72, 400);
    let countries = archive.countries(72, 14);
    let entities = archive.entities(72, 120);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let edges = archive.edges(&ids, 900);
    let topics = archive.topics();
    let monitors = archive.monitors();
    Read {
        counts: counts.articles,
        articles: articles.len(),
        pins: pins.len(),
        countries: countries.len(),
        entities: entities.len(),
        edges: edges.len(),
        topics: topics.len(),
        monitors: monitors.len(),
    }
}

/// The seeded copy, as every query should see it.
fn seeded() -> Read {
    Read {
        counts: 1,
        articles: 1,
        pins: 1,
        countries: 1,
        entities: 1,
        edges: 1,
        topics: 1,
        monitors: 1,
    }
}

/// A read is instant, and a write would move the mtime to *now*; waiting first means
/// a coarse file system cannot hide the difference by rounding it away.
fn settle() {
    std::thread::sleep(Duration::from_millis(1_100));
}

/// Make one directory unwritable, run `body`, and put the mode back whatever happens.
#[cfg(unix)]
fn with_directory_unwritable<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500))
        .expect("the directory is made unwritable");
    let outcome = body();
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    outcome
}

#[cfg(not(unix))]
fn with_directory_unwritable<T>(_dir: &Path, body: impl FnOnce() -> T) -> T {
    body()
}

// ---------------------------------------------------------------------------
// the archive cannot be written
// ---------------------------------------------------------------------------

#[test]
fn the_window_reads_a_copy_without_changing_a_byte_or_the_mtime() {
    let dir = temp_dir("untouched");
    let copy = dir.join("atlas.db");
    seed(&copy, "delete");
    settle();
    let before = facts(&copy);

    let archive = data::Archive::open(&copy).expect("the window opens the copy read-only");
    let read = read_everything(&archive);
    // The reads are real: an unchanged file because nothing was read would prove
    // nothing at all.
    assert_eq!(read, seeded(), "the queries answered from the seeded copy");
    drop(archive);

    let after = facts(&copy);
    assert_eq!(
        before.modified, after.modified,
        "the archive's mtime moved: something wrote through the window's connection"
    );
    assert_eq!(before.len, after.len, "the archive's size changed");
    assert_eq!(
        before.bytes, after.bytes,
        "the archive's bytes changed: a reader wrote the archive"
    );
    assert_eq!(
        side_files(&dir),
        Vec::<String>::new(),
        "reading left a journal or a WAL beside the archive: {:?}",
        listing(&dir)
    );

    drop(std::fs::remove_dir_all(&dir));
}

#[test]
fn the_window_does_not_put_the_archive_into_wal_mode() {
    // A WAL is not only a file that "appears": it is a mode recorded in the database
    // header, plus a `-wal` file. Both are checked, in both shapes an archive arrives
    // in — a fresh file (rollback journal) and the live archive's (WAL, because the
    // engines set it).
    let dir = temp_dir("journal-mode");
    let fresh = dir.join("fresh.db");
    seed(&fresh, "delete");
    let before = facts(&fresh);
    assert_eq!(
        before.format_version,
        (1, 1),
        "the fixture is not a rollback-journal database"
    );

    {
        let archive = data::Archive::open(&fresh).expect("the window opens the copy");
        let _ = read_everything(&archive);
    }
    let after = facts(&fresh);
    assert_eq!(
        after.format_version, before.format_version,
        "the window switched the archive's journal mode (header bytes 18/19)"
    );
    for unwanted in ["fresh.db-wal", "fresh.db-shm", "fresh.db-journal"] {
        assert!(
            !dir.join(unwanted).exists(),
            "the window created {unwanted}: {:?}",
            listing(&dir)
        );
    }

    // The other shape: an archive the engines already put in WAL mode.
    let live_shaped = dir.join("wal.db");
    seed(&live_shaped, "wal");
    settle();
    let before = facts(&live_shaped);
    {
        let archive = data::Archive::open(&live_shaped).expect("the window opens a WAL archive");
        assert_eq!(read_everything(&archive), seeded(), "the WAL copy was read");
    }
    let after = facts(&live_shaped);
    // Printed for `--nocapture`, so a report carries a measurement, not a claim.
    println!(
        "WAL-shaped copy: format {:?} -> {:?}, side files {:?}, WAL length {:?}",
        before.format_version,
        after.format_version,
        side_files(&dir),
        std::fs::metadata(dir.join("wal.db-wal")).map(|meta| meta.len()),
    );
    assert_eq!(
        after.format_version, (2, 2),
        "the window changed the journal mode of a WAL archive"
    );
    assert_eq!(before.bytes, after.bytes, "the window wrote a WAL-mode archive");
    assert_eq!(
        before.modified, after.modified,
        "the window touched a WAL-mode archive"
    );
    // SQLite creates the `-wal` and `-shm` a WAL read needs; what matters is what is
    // in them. A read-only connection cannot append a frame, so the WAL it creates
    // stays empty — one byte there would mean the window wrote.
    let wal = dir.join("wal.db-wal");
    if wal.exists() {
        assert_eq!(
            std::fs::metadata(&wal).expect("the WAL file").len(),
            0,
            "the window's connection put a frame in the WAL"
        );
    }

    drop(std::fs::remove_dir_all(&dir));
}

#[test]
fn the_window_reads_a_rollback_journal_archive_whose_directory_it_cannot_write() {
    // The structural version of "cannot write": a database whose *directory* refuses
    // new files. A rollback-journal read needs no write at all, so the window must
    // still open it and answer every query — a reader with any write in its path (a
    // WAL switch, a journal, a temp store) fails here instead.
    let dir = temp_dir("readonly-dir");
    let copy = dir.join("atlas.db");
    seed(&copy, "delete");
    settle();
    let before = facts(&copy);

    let (opened, read) = with_directory_unwritable(&dir, || match data::Archive::open(&copy) {
        Ok(archive) => {
            let read = read_everything(&archive);
            (Ok(()), Some(read))
        }
        Err(problem) => (Err(problem), None),
    });
    opened.unwrap_or_else(|problem| {
        panic!("a read-only connection needs no write permission: {problem}")
    });
    assert_eq!(read.expect("the reads ran"), seeded(), "the reads answered");

    let after = facts(&copy);
    assert_eq!(after.modified, before.modified, "the mtime moved");
    assert_eq!(after.bytes, before.bytes, "the bytes changed");
    assert_eq!(
        side_files(&dir),
        Vec::<String>::new(),
        "a side file appeared: {:?}",
        listing(&dir)
    );

    drop(std::fs::remove_dir_all(&dir));
}

#[test]
fn a_wal_archive_in_an_unwritable_directory_is_never_written() {
    // A WAL archive needs a `-shm` for its index, so unlike the rollback case it can
    // fail to read where the directory is unwritable (the ignored test below is that
    // defect). What must hold either way: the archive is not written by the attempt.
    let dir = temp_dir("wal-readonly-dir");
    let copy = dir.join("atlas.db");
    seed(&copy, "wal");
    settle();
    let before = facts(&copy);
    assert_eq!(side_files(&dir), Vec::<String>::new(), "the fixture has no side files");

    let (opened, read) = with_directory_unwritable(&dir, || match data::Archive::open(&copy) {
        Ok(archive) => (Ok(()), Some(read_everything(&archive))),
        Err(problem) => (Err(problem), None),
    });
    println!(
        "WAL archive, unwritable directory: opened={:?}, reads={:?}",
        opened.as_ref().map(|_| "ok"),
        read
    );

    let after = facts(&copy);
    assert_eq!(after.modified, before.modified, "the mtime moved");
    assert_eq!(after.bytes, before.bytes, "the bytes changed");
    assert_eq!(
        side_files(&dir),
        Vec::<String>::new(),
        "a side file appeared without write permission: {:?}",
        listing(&dir)
    );

    drop(std::fs::remove_dir_all(&dir));
}

#[test]
#[ignore = "data.rs: a WAL archive whose -shm cannot be created opens successfully and \
            reads as an EMPTY archive (measured: counts()=0 with a non-empty file), so the \
            window would draw '0 stories' over a full archive instead of a sentence. \
            Un-ignore this when data::Archive::open refuses a connection that sees no schema in a \
            non-empty file (SELECT count(*) FROM sqlite_master = 0 while the file is > 4096 \
            bytes) — it belongs in data.rs, not in a test."]
fn a_wal_archive_in_an_unwritable_directory_is_not_reported_as_empty() {
    let dir = temp_dir("wal-readonly-empty");
    let copy = dir.join("atlas.db");
    seed(&copy, "wal");
    settle();

    let (opened, read) = with_directory_unwritable(&dir, || match data::Archive::open(&copy) {
        Ok(archive) => (Ok(()), Some(read_everything(&archive))),
        Err(problem) => (Err(problem), None),
    });
    match (opened, read) {
        // Refusing with a sentence is an acceptable answer: the window says why.
        (Err(problem), _) => assert!(!problem.trim().is_empty(), "a refusal needs a sentence"),
        // Opening is fine too, as long as the archive is actually read.
        (Ok(()), Some(read)) => assert_eq!(
            read,
            seeded(),
            "the window opened the archive and reported it as empty"
        ),
        (Ok(()), None) => panic!("an opened archive answered nothing"),
    }

    drop(std::fs::remove_dir_all(&dir));
}

// ---------------------------------------------------------------------------
// the flags, and the one door that could open a writable file
// ---------------------------------------------------------------------------

#[test]
fn the_open_flags_ask_sqlite_for_read_only_and_nothing_else() {
    // The behavioural tests above would still pass if the window asked for a
    // *writable* connection and simply never wrote — so the flag itself is pinned, in
    // the crate's own source, which is the same source this test compiled.
    let source = include_str!("../src/data.rs");
    let rest = source
        .split_once("Connection::open_with_flags")
        .expect("the archive is opened with explicit flags")
        .1;
    let flags = match rest.find(';') {
        Some(ends) => &rest[..ends],
        None => rest,
    };
    assert!(
        flags.contains("SQLITE_OPEN_READ_ONLY"),
        "the archive is not opened read-only: {flags:?}"
    );
    for forbidden in [
        "SQLITE_OPEN_READ_WRITE",
        "SQLITE_OPEN_CREATE",
        "SQLITE_OPEN_MEMORY",
    ] {
        assert!(
            !flags.contains(forbidden),
            "the archive's flags contain {forbidden}: {flags:?}"
        );
    }
    // The second layer is a pragma this module must *set*: reading it back is not
    // enough, which is exactly the defect this assertion was written to catch (an
    // earlier revision ran `query_row("PRAGMA query_only", …)` and threw away the 0).
    assert!(
        source.contains("pragma_update(None, \"query_only\", true)")
            || source.contains("PRAGMA query_only = ON")
            || source.contains("PRAGMA query_only=ON"),
        "the window never switches `query_only` on"
    );
}

#[test]
fn a_path_that_is_not_a_file_is_refused_before_sqlite_sees_it() {
    // `open` passes `SQLITE_OPEN_URI`, which makes a path starting with `file:` a URI
    // with parameters — `?mode=rwc` asks SQLite to *create and write* the file the URI
    // names. What stops that today is the `path.exists()` check in `data::Archive::open`,
    // which runs on the literal string and refuses first. This test pins the refusal;
    // the report asks for the flag to go, because nothing passes URIs and the
    // `exists()` check is load-bearing only by accident.
    let dir = temp_dir("uri");
    let ghost = dir.join("ghost.db");
    let uri = format!("file:{}?mode=rwc", ghost.display());
    let message = match data::Archive::open(&uri) {
        Err(message) => message,
        Ok(_) => panic!("a URI asking for a writable file was opened: {uri}"),
    };
    assert!(
        message.contains("no archive at"),
        "unexpected refusal: {message}"
    );
    assert!(
        !ghost.exists(),
        "the window created a database file from a URI: {}",
        ghost.display()
    );
    drop(std::fs::remove_dir_all(&dir));
}
