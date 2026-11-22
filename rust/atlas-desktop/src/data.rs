//! The archive, read directly.
//!
//! One read-only SQLite connection to `data/atlas.db`, and one query per list the
//! window shows. Read-only is not politeness: the connection is opened with
//! `SQLITE_OPEN_READ_ONLY` **and** `PRAGMA query_only = ON`, so a view cannot write
//! the archive even by mistake — collection and classification stay where they were,
//! in the engines the model drives. (`PRAGMA query_only` is a switch, not a report:
//! reading it back is a check, and [`Archive::open`] fails if the switch did not take.)
//! `SQLITE_OPEN_URI` is left out on purpose: with it, the *string* decides what SQLite
//! does, and `file:…?mode=rwc` asks it to create and write the file — the read-only
//! promise would then rest on a pre-check instead of on the flag.
//!
//! [`Archive::open`] also refuses the archive that opens but reads as empty: a WAL file
//! whose `-shm` cannot be created answers every query from a blank schema, and "0
//! stories" over a full archive with no message anywhere is the worst answer this
//! window could give.
//!
//! Nothing here knows about HTTP. There is no server in this build: the queries the
//! old `/api/*` routes ran are the queries this file runs, in the same process that
//! draws the pixels.
//!
//! # An empty answer and a failed read
//!
//! Every read answers a list or an `Option`, never a `Result`: a page has to draw whatever
//! the archive can give it, and a column the collector renamed must not take the window down
//! with it. The price of that shape is that a failed statement and a window with no stories
//! both arrive as an empty list, and the two sentences a reader needs are different ones —
//! "no stories from Germany in the last 72 h" versus "the archive could not be read". So a
//! failure is recorded on the connection ([`Archive::last_error`] → [`ReadError`]) and every
//! `Err` arm in this file goes through [`Archive::failed`] or [`Archive::note`]; a page that is
//! about to print an empty answer asks first. A missing table is a failure too — that is how
//! an archive written by an older build reads — and the reads with a fallback shape
//! ([`Archive::country_stories`], [`Archive::countries`], [`Archive::counts`]) record nothing
//! when the older shape answered, because then nothing failed.
//!
//! # What a keystroke may cost
//!
//! Measured on the real archive (`data/atlas.db`, 1 435 articles, 269 inside the 72 h
//! window, 985 592 characters of body text), release build, one process, row output
//! kept (the whole read, which is what a keystroke pays), ten runs after three warm:
//!
//! | statement | p50 | p95 |
//! | --- | --- | --- |
//! | window read, empty search box (no `LIKE` at all) | 0.43 ms | 0.47 ms |
//! | window + search, one word that hits (93 rows) | 0.77 ms | 0.99 ms |
//! | window + search, one word that hits nothing (every body read) | 0.87 ms | 2.05 ms |
//!
//! The numbers come from [`tests::reading_the_real_archive_is_quick_enough_to_type_with`],
//! which is skipped in a debug build (a debug SQLite is not the shipped one); its
//! printed line is the source of this table. `counts()` was measured separately with a
//! prepared-statement harness, eight statements 0.076 ms → one statement 0.034 ms —
//! the point there is the round trips, not the milliseconds.
//!
//! `EXPLAIN QUERY PLAN` for all three search shapes is
//! `SEARCH a USING INDEX idx_articles_window (relevant=? AND dupe=? AND published>?)`:
//! the window is a range read and `LIMIT` stops it early. The `LIKE` terms are
//! evaluated only for the rows the window already selected, which is 1 MB of text —
//! inside a keystroke, so search is re-read as the reader types rather than on a
//! button. What that does *not* buy: a needle that hits nothing still reads every
//! body in the window, so the cost grows with the window, not with the match count.
//!
//! # The window bound
//!
//! [`window_start_iso`] subtracts the requested hours from the current UTC second
//! and formats the result in the archive's own shape (`2026-11-21T10:25:12Z`, UTC,
//! fixed width, `Z`), so `published >= bound` stays a string compare on one format —
//! the format every row in the archive is stored in (measured: all 1 435 rows match
//! `____-__-__T__:__:__Z`). It is arithmetic on seconds, not a calendar subtraction:
//! 72 h is 259 200 s whatever day boundary it crosses, and no local time zone or DST
//! rule can move it. [`window_start_iso_at`] takes "now" as an argument so the test
//! can pin that.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// One row of the article list, cut to what a list and a reading pane need.
#[derive(Clone, Default)]
pub struct Article {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub published: String,
    pub summary: String,
    pub content: String,
    pub country: String,
    pub url: String,
    pub full_text: bool,
}

/// A place on the map: one coordinate and how many stories named it.
#[derive(Clone)]
pub struct Pin {
    pub place: String,
    pub lat: f64,
    pub lon: f64,
    pub count: i64,
    /// The newest story filed at this place: what a click on the dot opens.
    ///
    /// A pin is a *place*, not a story — several stories share one dot — so the dot needs
    /// a destination, and the one a reader means by "the news here" is the newest. Chosen
    /// in SQL, ordered by `published` then `id`, so the answer is stable when two stories
    /// carry the same second.
    pub article: i64,
}

#[derive(Clone, Default)]
pub struct Entity {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub count: i64,
}

#[derive(Clone)]
pub struct Edge {
    pub a: i64,
    pub b: i64,
    pub weight: f64,
    pub label: String,
}

#[derive(Clone, Default)]
pub struct Topic {
    pub id: i64,
    pub name: String,
    pub keywords: String,
    pub active: bool,
    pub hits: i64,
    pub last_hit: String,
}

#[derive(Clone, Default)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub active: bool,
    pub cadence: i64,
    pub last_run: String,
    pub hits: i64,
}

/// One briefing: the model's prepared answer about a piece of the archive.
///
/// The table is `atlas-store`'s (`briefings`, one row per scope and window), and this is its
/// value in the window — the same ten fields, in the same order, spelled here because this
/// crate links no workspace crate (the same reason [`Article`] and [`Monitor`] are).
///
/// Two facts are not copied: the *shape* of `scope` is the queue's word (`window`, `country`,
/// `place`, `topic`, `monitor`, `entity` — see [`crate::views::brief::SCOPES`]), and the row
/// is written by the agent, never here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Briefing {
    pub scope: String,
    pub key: String,
    /// The window the briefing was written over: the number its footer says (`72 h`).
    pub hours: i64,
    /// The newest story the model was given, as an article id. A newer id in the archive is
    /// what makes a briefing stale ([`crate::views::brief::stale`]) — a *mark*, because the
    /// story's own timestamp can be backdated by its publisher and an id cannot.
    pub watermark: i64,
    /// When it was written, in the archive's stamp format (read by [`crate::views::when`]).
    pub created: String,
    /// One sentence: the situation.
    pub line: String,
    /// The facts, in the model's order, each possibly carrying `[Article #ID]` — the marker
    /// the card turns into a door to that story.
    pub bullets: Vec<String>,
    /// How many stories the model was given.
    pub articles: i64,
    /// The model that wrote it, by name.
    pub model: String,
    /// How long it took to write, in milliseconds. On the card nowhere: it is the agent's
    /// number, and a reader waiting on a prepared sentence is not helped by its cost.
    pub ms: i64,
}

#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub articles: i64,
    pub relevant: i64,
    pub entities: i64,
    pub edges: i64,
    pub links: i64,
    pub topics: i64,
    pub monitors: i64,
    pub alerts: i64,
}

/// A read that failed, kept so that "nothing to show" and "nothing could be read" are two
/// different answers.
///
/// Every method here answers a `Vec` or an `Option`, because a page has to draw whatever the
/// archive can give it: a table this checkout predates, a column whose type is not the one the
/// statement asked for, a statement SQLite refused — none of those may take the window down.
/// The cost of that shape is that a failed read looks exactly like an empty one at the call
/// site, and the two mean different things to a reader: *no stories in this window* is a fact
/// about the news, *nothing could be read* is a fact about the file, and a page that prints the
/// first sentence over the second is lying.
///
/// So a failure is remembered on the [`Archive`] (the last one wins, because the page asks
/// after the read it just made) and read back with [`Archive::last_error`];
/// [`Archive::clear_error`] is how a caller asks about one read rather than about the
/// session: clear, read, ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadError {
    /// Which read it was — the method's own name, so the sentence can say what could not be
    /// read ("the country list could not be read" is built from `country_stories`).
    pub read: &'static str,
    /// SQLite's message, verbatim: `no such table: entities`, `Invalid column type …`.
    pub message: String,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the {} read failed: {}", self.read, self.message)
    }
}

// ---------------------------------------------------------------------------
// the statements
// ---------------------------------------------------------------------------

/// The columns an [`Article`] row carries, in its field order.
///
/// `coalesce(nullif(s.name,''), …)`: an empty outlet name is not a name, so the
/// publisher the page itself declared is asked next and an unknown outlet is `""`
/// rather than `"?"` — the list prints "unknown source" for that, which is a
/// sentence about the archive; `"?"` reads like an outlet that is called that.
const ARTICLE_COLUMNS: &str = "a.id, coalesce(a.title,''), \
     coalesce(nullif(s.name,''), nullif(a.publisher,''), ''), coalesce(a.published,''), \
     coalesce(a.summary,''), coalesce(a.content,''), coalesce(a.iso,''), coalesce(a.url,''), \
     coalesce(a.full_text,0)";

/// The window predicate, and the first parameter of every article statement.
///
/// `published` is TEXT in the archive's own format, so this is a string compare
/// against [`window_start_iso`]. It stays on `published` and not
/// `coalesce(published, fetched)`: `idx_articles_window` starts with
/// `(relevant, dupe, published DESC)`, and an expression over the column would throw
/// that range read away on every keystroke. An article without a publication date is
/// filed as "now" by the collector, so there is nothing to fall back for.
const ARTICLE_WINDOW: &str = "a.relevant = 1 AND a.dupe = 0 AND a.published >= ?1";

/// The label a place is *named* by, and the only thing that decides which stories are
/// one place's.
///
/// It is the expression [`Archive::pins`] groups by (`… AS label` in its `w` CTE), spelled
/// once more rather than restated in prose: the map's dot and the page it opens must not
/// disagree about what belongs together, and two copies of this expression that drift are
/// exactly how they would. `place` first, because the archive's own place name is what the
/// reader clicked; `iso` second, for a story the geocoder only placed in a country; `''`
/// last, for a story that names no place at all — and `''` is not a page, which is why
/// [`Archive::place_stories`] refuses it and [`Archive::pins`] drops those rows.
///
/// Measured agreement, 72 h window of the shipped archive: the pins labelled "United
/// States", "China" and "Washington" carry 79, 78 and 14 stories, and
/// `place_stories` returns 79, 78 and 14 rows for those labels
/// (`tests/place_stories.rs` re-checks it on whatever archive is installed).
const PLACE_LABEL: &str = "coalesce(nullif(a.place, ''), nullif(a.iso, ''), '')";

/// The most keys one [`Archive::articles_by_ids`] call will read.
///
/// The statement carries one `?` per key, so this is also the bound on the parameter
/// list: 600 dots is more than the map draws at any zoom (the cluster rule merges them
/// long before that), and it stays far below SQLite's variable limit.
const MAX_ID_LOOKUPS: usize = 600;

/// The search predicate: one escaped `LIKE` per column the list can show a hit in.
///
/// `ESCAPE '\'` is what makes [`like_pattern`]'s escaping real; without it a reader
/// who types `50%` or `a_b` would be writing a wildcard, not a search.
const ARTICLE_SEARCH: &str = " AND (a.title LIKE ?2 ESCAPE '\\' \
     OR a.summary LIKE ?2 ESCAPE '\\' OR a.content LIKE ?2 ESCAPE '\\')";

/// The eight numbers the status rail shows, as one statement.
///
/// Eight separate `SELECT count(*)` calls were eight prepares and eight round trips
/// per reload for the same eight numbers (measured 0.076 ms against 0.034 ms); one
/// statement answers them in one pass over the schema's indexes.
const COUNT_SQL: &str = "SELECT (SELECT count(*) FROM articles), \
     (SELECT count(*) FROM articles WHERE relevant = 1), (SELECT count(*) FROM entities), \
     (SELECT count(*) FROM edges), (SELECT count(*) FROM article_links), \
     (SELECT count(*) FROM topics), (SELECT count(*) FROM monitors), \
     (SELECT count(*) FROM alerts)";

/// [`COUNT_SQL`] for an archive that has no monitor half yet.
///
/// `monitors`/`alerts` are created by the collector's schema step, so an archive
/// written by an older build has neither. A missing table there must not zero the
/// article count — that would print "0 stories" over an archive full of stories.
const COUNT_SQL_BASE: &str = "SELECT (SELECT count(*) FROM articles), \
     (SELECT count(*) FROM articles WHERE relevant = 1), (SELECT count(*) FROM entities), \
     (SELECT count(*) FROM edges), (SELECT count(*) FROM article_links), \
     (SELECT count(*) FROM topics), 0, 0";

pub struct Archive {
    conn: Connection,
    pub path: PathBuf,
    /// The last read that failed on this connection, or `None`.
    ///
    /// `RefCell`, not a plain field, because every read here takes `&self`: a failure is a
    /// note about the connection, and making every method `&mut self` would ripple through
    /// every page for a value only the status row asks for.
    read_error: RefCell<Option<ReadError>>,
}

impl Archive {
    /// Open the archive read-only. A missing file is a sentence, not a panic: the
    /// window still opens and says what is wrong in the status row.
    ///
    /// Every sentence here names the file through [`crate::assets::label`], not through
    /// `Path::display`: this is the text the status row prints, and a home directory on
    /// that row tells a passer-by who the reader is.
    pub fn open(path: impl AsRef<Path>) -> Result<Archive, String> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(format!(
                "no archive at {} — start ATLAS once so it is created",
                crate::assets::label(&path)
            ));
        }
        // The lock is the flag set, not a missing flag: this build compiles SQLite with
        // `SQLITE_USE_URI` (`libsqlite3-sys-0.30.1/build.rs:137`), so a `file:…?mode=rwc`
        // string is parsed as a URI either way and `READ_ONLY` is what answers
        // `access mode not allowed`. Measured, both spellings, in
        // `tests/no_spawn_no_socket.rs::the_archive_is_read_only_by_flags_and_a_path_cannot_change_that`
        // — which is also why `SQLITE_OPEN_URI` is not passed: the flag would change
        // nothing and the rule should read as one rule. The `exists()` check above stays
        // because a missing archive deserves a sentence, not a SQLite code.
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("cannot open {}: {error}", crate::assets::label(&path)))?;
        // `PRAGMA query_only` is set, not read: it used to be *queried* and the value
        // thrown away, so the comment above claimed a guard that was never switched
        // on. Reading it back is the check — a connection that would not take the
        // setting (a future driver change, a proxy) fails here instead of writing.
        conn.pragma_update(None, "query_only", true)
            .map_err(|error| format!("cannot set query_only: {error}"))?;
        let enforced: i64 = conn
            .query_row("PRAGMA query_only", [], |row| row.get(0))
            .map_err(|error| format!("cannot read query_only back: {error}"))?;
        if enforced == 0 {
            return Err(format!(
                "{} opened read-write — refusing to use it",
                crate::assets::label(&path)
            ));
        }
        // An archive in WAL mode needs a `-shm` file to hold the write-ahead log's index.
        // When the directory cannot be written (a checkout owned by another user, a
        // read-only mount) the open still **succeeds** and every query answers from an
        // empty schema: the window would draw "0 stories" over an archive full of
        // stories, and no message anywhere. Measured on a seeded WAL copy in an
        // unwritable directory: `counts()` came back 0 with 20 KiB on disk. So the
        // question is asked here instead: no table at all, in a file larger than the
        // 4096-byte header page, is that state and not an empty archive — a database
        // with no tables yet is 0 bytes on disk.
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .unwrap_or(0);
        let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if tables == 0 && bytes > 4096 {
            return Err(format!(
                "{} reads as empty though its file holds {bytes} bytes — a write-ahead \
                 log needs a folder it can write beside it; check that, then start ATLAS \
                 again",
                crate::assets::label(&path)
            ));
        }
        Ok(Archive { conn, path, read_error: RefCell::new(None) })
    }

    /// The last read that failed on this archive, or `None` when every read answered.
    ///
    /// This is the difference between "no stories in this window" and "nothing could be read":
    /// both answer an empty list, and a page that has just drawn the empty list asks here
    /// before it prints a sentence about the news. The record is per archive and not per call,
    /// so the read order matters — [`Archive::clear_error`] is how a caller makes the question
    /// about one read.
    pub fn last_error(&self) -> Option<ReadError> {
        self.read_error.borrow().clone()
    }

    /// Forget the last failure. Clear it, make one read, then ask [`Archive::last_error`] and
    /// the answer is about that read alone.
    pub fn clear_error(&self) {
        self.read_error.replace(None);
    }

    /// Remember a failed read. The last one wins: a page asks after the read it just made.
    fn note(&self, read: &'static str, error: &rusqlite::Error) {
        self.read_error.replace(Some(ReadError { read, message: error.to_string() }));
    }

    /// The empty list a failed read stands for, with the failure recorded first.
    ///
    /// Every `Err` arm in this file goes through here or [`Archive::note`]: that is what makes
    /// the swallow visible to a page instead of silent.
    fn failed<T>(&self, read: &'static str, error: &rusqlite::Error) -> Vec<T> {
        self.note(read, error);
        Vec::new()
    }

    /// One statement's rows, with both kinds of failure recorded: the statement that would not
    /// run, and a row whose columns are not the types [`article_row`] and its siblings asked
    /// for (a hand-edited archive, a column the collector changed).
    ///
    /// The rows that *did* decode are kept — a page can still draw what it read — and the
    /// failure is what tells it the list is short for a reason that is not the window. The
    /// rows kept are the ones read before the failing row: rusqlite ends the iteration at a
    /// row it could not decode, and `tests::a_failed_read_is_not_an_empty_read` pins that.
    fn rows<'a, T, F>(
        &self,
        read: &'static str,
        rows: rusqlite::Result<rusqlite::MappedRows<'a, F>>,
    ) -> Vec<T>
    where
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let mut out = Vec::new();
        match rows {
            Ok(rows) => {
                for row in rows {
                    match row {
                        Ok(value) => out.push(value),
                        Err(error) => self.note(read, &error),
                    }
                }
            }
            Err(error) => self.note(read, &error),
        }
        out
    }

    /// The eight numbers the rail shows, in one query.
    pub fn counts(&self) -> Counts {
        match self.read_counts(COUNT_SQL) {
            Ok(counts) => counts,
            // The monitors half is the collector's newest step, so the first statement is
            // allowed to fail on an older file. The fallback answers the six counts that do
            // exist, and only a failure of *that* is recorded: a healthy older archive must
            // not look broken to the status row.
            Err(_) => match self.read_counts(COUNT_SQL_BASE) {
                Ok(counts) => counts,
                Err(error) => {
                    self.note("counts", &error);
                    Counts::default()
                }
            },
        }
    }

    fn read_counts(&self, sql: &str) -> rusqlite::Result<Counts> {
        self.conn.query_row(sql, [], |row| {
            Ok(Counts {
                articles: row.get(0)?,
                relevant: row.get(1)?,
                entities: row.get(2)?,
                edges: row.get(3)?,
                links: row.get(4)?,
                topics: row.get(5)?,
                monitors: row.get(6)?,
                alerts: row.get(7)?,
            })
        })
    }

    /// The newest relevant stories inside the window, newest first, optionally
    /// narrowed to the reader's own text.
    ///
    /// An empty search box is a different statement from a filled one, not the same
    /// statement with a `'%%'` sentinel: with no needle there is no `LIKE` term to
    /// evaluate for every row, and no sentinel that a needle of `%` could collide
    /// with. [`article_sql`] is the one place the two shapes are built.
    pub fn articles(&self, hours: i64, limit: i64, needle: &str) -> Vec<Article> {
        let since = window_start_iso(hours);
        let needle = needle.trim();
        let searching = !needle.is_empty();
        let mut statement = match self.conn.prepare(&article_sql(searching)) {
            Ok(statement) => statement,
            Err(error) => return self.failed("articles", &error),
        };
        let rows = if searching {
            statement.query_map(
                rusqlite::params![since, like_pattern(needle), limit],
                article_row,
            )
        } else {
            statement.query_map(rusqlite::params![since, limit], article_row)
        };
        self.rows("articles", rows)
    }

    /// The stories with these ids, newest first — read by id, whatever window or search
    /// the list is showing.
    ///
    /// The rail's list is capped ([`Archive::articles`] with a limit), and a pin's
    /// destination is chosen by the archive, not by that cap: on a busy window the newest
    /// story at a place can be older than the last row of the list, so a dot on the map
    /// would open "That story is no longer in the window." A click on a dot is a request
    /// for one particular story, so the page fetches that story by its key instead of
    /// hoping the list already holds it.
    ///
    /// `relevant`/`dupe` still apply (a dot is drawn from relevant stories, so its
    /// destination is one), but the window and the search do not: the reader asked for
    /// *this* story, and the handful of primary-key lookups is cheaper than a second
    /// window scan. Duplicate ids are read once, and the id list is capped so a caller
    /// can never build a statement with more parameters than SQLite allows.
    pub fn articles_by_ids(&self, ids: &[i64]) -> Vec<Article> {
        let mut wanted: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
        wanted.sort_unstable();
        wanted.dedup();
        wanted.truncate(MAX_ID_LOOKUPS);
        if wanted.is_empty() {
            return Vec::new();
        }
        let marks = vec!["?"; wanted.len()].join(", ");
        let sql = format!(
            "SELECT {ARTICLE_COLUMNS} FROM articles a LEFT JOIN sources s ON s.id = a.source_id \
             WHERE a.relevant = 1 AND a.dupe = 0 AND a.id IN ({marks}) \
             ORDER BY a.published DESC, a.id DESC"
        );
        let mut statement = match self.conn.prepare(&sql) {
            Ok(statement) => statement,
            Err(error) => return self.failed("articles_by_ids", &error),
        };
        let rows = statement.query_map(rusqlite::params_from_iter(wanted.iter()), article_row);
        self.rows("articles_by_ids", rows)
    }

    /// The places the stories of the window come from, weighted by how many mention
    /// them.
    ///
    /// A pin is a *place the archive named*, not a coordinate: grouping by rounded
    /// coordinates merged "United States" (80 stories) and "Washington" (15) into one
    /// pin of 95 labelled `USA`, because both geocode to the same point, and it split
    /// one place in two as soon as two of its stories were geocoded a hundred metres
    /// apart. The label is the place, the coordinate is the centroid of the stories
    /// that named it, rounded to four decimals — display precision (~11 m), no longer
    /// the thing that decides which stories share a pin.
    ///
    /// `0,0` is the archive saying "no place" (measured: 92 rows, a third of the
    /// window), and the Gulf of Guinea is not a story. It is dropped **in SQL, before
    /// `LIMIT`**: filtered in Rust afterwards it would eat the limit and hand back
    /// fewer pins than were asked for.
    pub fn pins(&self, hours: i64, limit: i64) -> Vec<Pin> {
        let since = window_start_iso(hours);
        // The fifth column is the newest story in the same group: the pin's destination, so
        // that clicking a dot on the map opens something. One window-function pass over the
        // window computes the group size, the mean position and the ranking together; the
        // correlated subquery this replaced re-scanned the whole window once per group and
        // cost nearly four times as much (measured over the shipped archive, 120-hour window:
        // 3.97 ms before, 1.03 ms here — the plain group-by both replaced was 0.42 ms).
        let sql = "WITH w AS ( \
                     SELECT id, published, lat, lon, coalesce(iso, '') AS iso, \
                            coalesce(nullif(place, ''), nullif(iso, ''), '') AS label \
                     FROM articles \
                     WHERE relevant = 1 AND dupe = 0 AND published >= ?1 \
                     AND lat IS NOT NULL AND lon IS NOT NULL \
                     AND NOT (lat = 0 AND lon = 0) \
                     AND lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180 \
                   ), ranked AS ( \
                     SELECT label, id, \
                            row_number() OVER (PARTITION BY label, iso \
                                               ORDER BY published DESC, id DESC) AS rn, \
                            count(*) OVER (PARTITION BY label, iso) AS n, \
                            avg(lat) OVER (PARTITION BY label, iso) AS alat, \
                            avg(lon) OVER (PARTITION BY label, iso) AS alon \
                     FROM w \
                   ) \
                   SELECT label, round(alat, 4), round(alon, 4), n, id \
                   FROM ranked WHERE rn = 1 ORDER BY n DESC, label ASC LIMIT ?2";
        let mut statement = match self.conn.prepare(sql) {
            Ok(statement) => statement,
            Err(error) => return self.failed("pins", &error),
        };
        let rows = statement.query_map(rusqlite::params![since, limit], |row| {
            Ok(Pin {
                place: row.get(0)?,
                lat: row.get(1)?,
                lon: row.get(2)?,
                count: row.get(3)?,
                article: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            })
        });
        let pins: Vec<Pin> = self.rows("pins", rows);
        pins.into_iter()
            // The bounds are SQL's now; this only keeps a non-finite value (an
            // archive somebody hand-edited) from reaching the projection maths.
            .filter(|pin| pin.lat.is_finite() && pin.lon.is_finite())
            .collect()
    }

    /// The stories a place is, inside the window, newest first — the list a dot on the
    /// map opens, and the list a row of the country rollup opens.
    ///
    /// The label is [`PLACE_LABEL`], the expression [`Archive::pins`] groups by, so a dot
    /// carrying `count` and this page cannot disagree about what belongs together: the
    /// same label is the same set of rows. Nothing here re-reads the geocoder's
    /// coordinates — a story that names "Washington" is Washington's whether or not it
    /// carries a point, which is why this can be one row longer than a pin for the same
    /// label (a pin only counts rows it can draw).
    ///
    /// `relevant`/`dupe`/`published` are [`ARTICLE_WINDOW`]'s, so the rows agree with the
    /// Articles page by construction: a story the list would not show is not a place's
    /// story either. `published DESC, a.id DESC` is [`Archive::articles`]'s order, so a
    /// page of the same stories reads the same way wherever it was opened.
    ///
    /// An empty label is not a place (it is the archive saying "no place"), an unknown
    /// label is simply a place with no stories this window, and a non-positive limit asks
    /// for nothing: each answers an empty list rather than an error, because "this dot has
    /// no stories" is a sentence the page prints.
    ///
    /// Measured on the shipped archive, 72 h window (fastest of twelve runs, on a machine
    /// that was also building): "United States" 79 rows in 0.64-0.89 ms, "China" 78 in
    /// 0.79-1.20 ms, "Washington" 14 in 0.25-0.27 ms — the same three counts
    /// [`Archive::pins`] reports for those labels. `EXPLAIN QUERY PLAN`: `SEARCH a USING
    /// INDEX idx_articles_window`, so the window is a range read and the label is a
    /// residual filter tested per row (no index can serve an expression over two columns);
    /// the `a.id DESC` tiebreak costs one temp B-tree over the rows of one place, which the
    /// cost above already contains.
    pub fn place_stories(&self, label: &str, hours: i64, limit: i64) -> Vec<Article> {
        let label = label.trim();
        if label.is_empty() || limit <= 0 {
            return Vec::new();
        }
        let since = window_start_iso(hours);
        let sql = format!(
            "SELECT {ARTICLE_COLUMNS} FROM articles a \
             LEFT JOIN sources s ON s.id = a.source_id \
             WHERE {PLACE_LABEL} = ?2 AND {ARTICLE_WINDOW} \
             ORDER BY a.published DESC, a.id DESC LIMIT ?3"
        );
        let mut statement = match self.conn.prepare(&sql) {
            Ok(statement) => statement,
            Err(error) => return self.failed("place_stories", &error),
        };
        let rows = statement.query_map(rusqlite::params![since, label, limit], article_row);
        self.rows("place_stories", rows)
    }

    /// The stories of one country, inside the window, newest first — the list a chip of the
    /// country rollup opens, and the list a country's mark on the plate opens.
    ///
    /// A country is **every** place the archive filed a story under, not only the primary
    /// one. `articles.iso` is a story's own country; `article_countries` holds the further
    /// places it names (a summit in Kyiv filed under `UKR` that also talks about `DEU` has a
    /// row for `DEU` with `is_primary = 0`). Reading the column alone — which is what this
    /// did before — put such a story in no German list at all: measured on the shipped
    /// archive, 72 h window, the rollup said `DEU 3` while this returned **0** for it, so a
    /// country the map named had a door that opened nothing. The predicate is now
    /// `a.iso = ?2 OR EXISTS (row in article_countries)`, which is one set of stories: a row
    /// counted once whether the country is its first place or its fourth.
    ///
    /// [`Archive::countries`] counts the same pairs, so the number on a chip and the length
    /// of this list are the same number: measured, 56 countries and 574 (story, country)
    /// pairs in the 72 h window, `CHN 164` and `USA 153` at the top, `DEU 3` where the old
    /// read said 0.
    ///
    /// The window, the order and the limit are [`Archive::articles`]', so a country's page
    /// reads like every other list; `relevant`/`dupe`/`published` are [`ARTICLE_WINDOW`]'s,
    /// so a story the list would not show is not here either. An empty ISO, a country with
    /// no story this window and a non-positive limit each answer an empty list.
    ///
    /// Measured on the shipped archive, 72 h window (fastest of twelve runs): `CHN` 164 rows
    /// in 0.75 ms, `USA` 153 in 0.72 ms, `DEU` 3 in 0.35 ms. `EXPLAIN QUERY PLAN`: the window
    /// range read on `idx_articles_window` (1 440 rows of the table), then the `EXISTS` as a
    /// primary-key lookup per row — `SEARCH c USING COVERING INDEX
    /// sqlite_autoindex_article_countries_1 (article_id=? AND iso=?)`, which is the column
    /// order of `article_countries`' own key — and one temp B-tree for the `a.id DESC`
    /// tiebreak. The old single-column read cost 0.10–1.26 ms on the same rows; the `EXISTS`
    /// adds 0.35 ms to the worst of them.
    pub fn country_stories(&self, iso: &str, hours: i64, limit: i64) -> Vec<Article> {
        let iso = iso.trim();
        if iso.is_empty() || limit <= 0 {
            return Vec::new();
        }
        let since = window_start_iso(hours);
        // Two shapes of one read, and the second is only for a file that predates the second
        // places: `article_countries` is the collector's table, and a window over an older
        // archive must answer the stories it can see rather than fail to prepare (the same
        // rule [`Archive::monitor_stories`] keeps for a file without the monitor tables). On
        // the shipped archive the first prepare succeeds and the fallback is never built.
        let both = format!(
            "SELECT {ARTICLE_COLUMNS} FROM articles a \
             LEFT JOIN sources s ON s.id = a.source_id \
             WHERE (a.iso = ?2 OR EXISTS (SELECT 1 FROM article_countries c \
                    WHERE c.article_id = a.id AND c.iso = ?2)) \
             AND {ARTICLE_WINDOW} \
             ORDER BY a.published DESC, a.id DESC LIMIT ?3"
        );
        let primary = format!(
            "SELECT {ARTICLE_COLUMNS} FROM articles a \
             LEFT JOIN sources s ON s.id = a.source_id \
             WHERE a.iso = ?2 AND {ARTICLE_WINDOW} \
             ORDER BY a.published DESC, a.id DESC LIMIT ?3"
        );
        let mut statement = match self.conn.prepare(&both) {
            Ok(statement) => statement,
            // An archive from before `article_countries` existed has no second places to
            // read: the primary column is then the whole answer, and the fallback's own
            // failure is the one recorded. A read that falls back and succeeds records
            // nothing — the older shape answered the question it could answer.
            Err(_) => match self.conn.prepare(&primary) {
                Ok(statement) => statement,
                Err(error) => return self.failed("country_stories", &error),
            },
        };
        let rows = statement.query_map(rusqlite::params![since, iso, limit], article_row);
        self.rows("country_stories", rows)
    }

    /// The countries the window covers, by number of stories, for the country rollup and for
    /// the marks the plate draws — **every place a story names**, not only its first.
    ///
    /// `articles.iso` says where a story is filed; `article_countries` says where else it
    /// goes (see [`Archive::country_stories`]). Counting the column alone under-counted the
    /// map: measured on the shipped archive, 72 h window, 18 countries and `USA 105` /
    /// `CHN 89`; counting both, **56 countries** and `CHN 164` / `USA 153` / `RUS 43` — 574
    /// (story, country) pairs. A story that names four countries is counted once in each,
    /// because the two sources are `UNION`ed as *pairs*, not concatenated: a story whose
    /// primary country also has a row for it (`is_primary = 1`, 367 rows of the file) is one
    /// story, not two.
    ///
    /// The order is the count, then the code, so a limit can never make two equal counts
    /// swap places between two reads of an unchanged archive; `published`/`relevant`/`dupe`
    /// are [`ARTICLE_WINDOW`]'s, so a country's number is the length of its list
    /// ([`Archive::country_stories`], 0.7–1.4 ms and the same rows).
    ///
    /// Measured on the shipped archive, 72 h window (fastest of twelve runs): 56 rows in
    /// 0.62 ms. `EXPLAIN QUERY PLAN`: two window range reads on `idx_articles_window` (one
    /// of them joined to `article_countries` through the covering key
    /// `(article_id, iso)`), the `UNION` as one temp B-tree over 1 440 + 570 candidates, then
    /// the group and the order over 56 rows.
    pub fn countries(&self, hours: i64, limit: i64) -> Vec<(String, i64)> {
        let since = window_start_iso(hours);
        // The primary-only fallback is for an archive that predates `article_countries` — see
        // [`Archive::country_stories`]; on the shipped file the first prepare succeeds.
        let both = "SELECT iso, count(*) AS n FROM ( \
                 SELECT a.id AS id, a.iso AS iso FROM articles a \
                 WHERE a.relevant = 1 AND a.dupe = 0 AND a.published >= ?1 \
                   AND coalesce(a.iso, '') <> '' \
                 UNION \
                 SELECT a.id, c.iso FROM article_countries c \
                 JOIN articles a ON a.id = c.article_id \
                 WHERE a.relevant = 1 AND a.dupe = 0 AND a.published >= ?1 \
                   AND coalesce(c.iso, '') <> '' \
             ) GROUP BY iso ORDER BY n DESC, iso ASC LIMIT ?2";
        let primary = "SELECT coalesce(a.iso, ''), count(*) FROM articles a \
             WHERE a.relevant = 1 AND a.dupe = 0 AND a.published >= ?1 \
               AND coalesce(a.iso, '') <> '' GROUP BY 1 ORDER BY 2 DESC, 1 ASC LIMIT ?2";
        let mut statement = match self.conn.prepare(both) {
            Ok(statement) => statement,
            // Primary-only fallback for an archive that predates `article_countries`; the
            // failure is recorded only when the fallback fails too (see
            // [`Archive::country_stories`]).
            Err(_) => match self.conn.prepare(primary) {
                Ok(statement) => statement,
                Err(error) => return self.failed("countries", &error),
            },
        };
        let rows = statement.query_map(rusqlite::params![since, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        });
        self.rows("countries", rows)
    }

    /// The graph's nodes: the strongest entities in the window **that have a relation**.
    ///
    /// Mention count alone is not a graph. Measured on the archive in `data/` — 4 539
    /// entities in a 72 h window and 7 338 relations — the 120 most-mentioned entities
    /// carried 232 relations between them and **84** of the 120 had none at all: two
    /// thirds of the canvas was unconnected dots, which reads as "the relations are
    /// missing". Requiring a relation at either end and keeping the same order draws
    /// **974** relations (4.2×) with no isolated node. An entity the page cannot explain
    /// does not deserve a slot that a connected one could use.
    ///
    /// The `EXISTS` pair is why the order stays on the bare `count` column rather than on
    /// `coalesce(count,0)`: the bare column walks `idx_entities_count` and stops as soon
    /// as 120 rows have passed both filters, while the wrapped form sorts all 4 539 rows
    /// before the limit. `tests/query_budget.rs` measures this read at 0.40 ms (minimum
    /// of forty; 0.12 ms before the filter, inside the same 1.5 ms ceiling). Ordering the
    /// nodes by degree instead would draw 1 234 relations rather than 974, but it costs
    /// 2–4 ms (SQLite's own timer, twenty runs): the connectivity filter buys most of the
    /// picture for a fifth of that.
    pub fn entities(&self, hours: i64, limit: i64) -> Vec<Entity> {
        let since = window_start_iso(hours);
        let sql = "SELECT id, coalesce(name,''), coalesce(type,''), coalesce(count,0) FROM entities \
                   WHERE coalesce(last_seen,'') >= ?1 \
                     AND (EXISTS (SELECT 1 FROM edges e WHERE e.a = entities.id) \
                          OR EXISTS (SELECT 1 FROM edges e WHERE e.b = entities.id)) \
                   ORDER BY count DESC LIMIT ?2";
        let mut statement = match self.conn.prepare(sql) {
            Ok(statement) => statement,
            Err(error) => return self.failed("entities", &error),
        };
        let rows = statement.query_map(rusqlite::params![since, limit], |row| {
            Ok(Entity {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                count: row.get(3)?,
            })
        });
        self.rows("entities", rows)
    }

    /// The relations among a set of entities: both ends must be in the set, so a line
    /// never points at a node that is not drawn.
    ///
    /// The ids are bound, never interpolated: an id list is data, and building
    /// `IN (1,2,3…)` as text made the statement's shape depend on how many entities
    /// happened to be in the graph — a list long enough to hit SQLite's expression
    /// tree limit would have turned the graph page blank with no error anywhere. Two
    /// parameter runs of the same ids, then the limit.
    ///
    /// The `+` on `a` is the whole of the performance work here, and it is a plan
    /// decision rather than a value. With both lists on bare columns SQLite builds the
    /// *pair* predicate `(a=? AND b=?)` out of the primary key, so a 120-node graph
    /// becomes 120 × 120 = 14 400 index seeks to find 232 rows. The unary `+` — SQLite's
    /// own way of saying "not this column" — leaves one seek per id and filters the other
    /// end against the same set.
    ///
    /// Measured on the archive in `data/`, 40 runs per shape in one process, statement
    /// prepared per call as below; minimum of the forty (the least-disturbed run) and, in
    /// brackets, the median, which on this machine is the build queue more than the query
    /// (3.65 [11.6] ms before, 0.63 [1.4] ms after):
    ///
    /// | shape                             | plan                              | min     |
    /// |-----------------------------------|-----------------------------------|---------|
    /// | `a IN (…) AND b IN (…)` (before)  | `sqlite_autoindex_edges_1 (a=? AND b=?)` | 3.65 ms |
    /// | `a IN (…) AND +b IN (…)`          | `sqlite_autoindex_edges_1 (a=?)`  | 0.93 ms |
    /// | `+a IN (…) AND b IN (…)` (this)   | `idx_edges_b (b=?)`               | 0.63 ms |
    /// | `+a IN (…) AND +b IN (…)`         | `SCAN edges`                      | 1.33 ms |
    ///
    /// So the hint cannot make it worse than it was even if `idx_edges_b` ever went
    /// away: what it removes is the product of two lists, and a plain `SCAN edges` is
    /// still 2.7 times better than that product.
    ///
    /// The rest of the shape stayed because three other probes (30 runs each) said the
    /// other candidates are not where the cost is: `count(*)` of the two predicates with
    /// no row decoding measured 3.36 ms, so it was never the decode or the sort; one
    /// statement reused across calls differed from prepared-per-call by less than 0.3 ms;
    /// and a `json_each(?1)` form measured the same as the two bound runs. Neither
    /// earned a statement cache, and the ids stay bound where `tests/sql_safety.rs`
    /// reads them.
    pub fn edges(&self, ids: &[i64], limit: i64) -> Vec<Edge> {
        if ids.is_empty() {
            return Vec::new();
        }
        let marks = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT a, b, coalesce(weight,1.0), coalesce(label,'') FROM edges \
             WHERE +a IN ({marks}) AND b IN ({marks}) ORDER BY weight DESC LIMIT ?"
        );
        let mut statement = match self.conn.prepare(&sql) {
            Ok(statement) => statement,
            Err(error) => return self.failed("edges", &error),
        };
        let mut params: Vec<i64> = Vec::with_capacity(ids.len() * 2 + 1);
        params.extend_from_slice(ids);
        params.extend_from_slice(ids);
        params.push(limit);
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(Edge {
                a: row.get(0)?,
                b: row.get(1)?,
                weight: row.get(2)?,
                label: row.get(3)?,
            })
        });
        self.rows("edges", rows)
    }

    pub fn topics(&self) -> Vec<Topic> {
        let mut statement = match self.conn.prepare(
            "SELECT id, coalesce(name,''), coalesce(keywords,''), coalesce(active,0), \
             coalesce(hits,0), coalesce(last_hit,'') FROM topics ORDER BY active DESC, hits DESC",
        ) {
            Ok(statement) => statement,
            Err(error) => return self.failed("topics", &error),
        };
        let rows = statement.query_map([], |row| {
            Ok(Topic {
                id: row.get(0)?,
                name: row.get(1)?,
                keywords: row.get(2)?,
                active: row.get::<_, i64>(3)? != 0,
                hits: row.get(4)?,
                last_hit: row.get(5)?,
            })
        });
        self.rows("topics", rows)
    }

    /// The stories of one topic inside the window, newest first.
    ///
    /// The Topics page lists a topic's stories under the row that is open, and it lists
    /// them exactly as the Articles page does — the same [`ARTICLE_COLUMNS`], the same
    /// [`article_row`], so a row here carries the title, source, stamp and text a reader
    /// already knows how to read, and the click can hand the story to the reading pane
    /// without a second lookup.
    ///
    /// The join is `article_topics` (the archive's own answer to "which topic did this
    /// story match"), not the terms re-matched here: the table is what the classifier
    /// wrote, and re-deriving it from `keywords` in the window would be a second
    /// implementation of the rule that decides what a topic is.
    ///
    /// The window and the `relevant`/`dupe` pair are [`ARTICLE_WINDOW`]'s, so the rows
    /// agree with the Articles page by construction: a story that the list would not show
    /// is not in a topic's stories either. `published` is bound to `?1` by that predicate,
    /// which is why the topic is `?2` and the limit `?3`. Measured on the shipped archive
    /// (72 h window): 128 / 202 / 116 rows for topics 1 / 2 / 3, and 0.07–0.22 ms warm,
    /// 0.74 ms cold for the ten-row page (`EXPLAIN QUERY PLAN` drives from
    /// `idx_articles_window` and probes the `article_topics` primary key).
    pub fn topic_articles(&self, topic_id: i64, hours: i64, limit: i64) -> Vec<Article> {
        if topic_id <= 0 || limit <= 0 {
            return Vec::new();
        }
        let since = window_start_iso(hours);
        let sql = format!(
            "SELECT {ARTICLE_COLUMNS} FROM article_topics t \
             JOIN articles a ON a.id = t.article_id \
             LEFT JOIN sources s ON s.id = a.source_id \
             WHERE t.topic_id = ?2 AND {ARTICLE_WINDOW} \
             ORDER BY a.published DESC, a.id DESC LIMIT ?3"
        );
        let mut statement = match self.conn.prepare(&sql) {
            Ok(statement) => statement,
            // An archive from before `article_topics` existed answers "no stories", which is
            // a sentence the page can print; the failure is recorded so the page can tell
            // that sentence from a topic whose stories have aged out ([`Archive::last_error`]).
            Err(error) => return self.failed("topic_articles", &error),
        };
        let rows = statement.query_map(
            rusqlite::params![since, topic_id, limit],
            article_row,
        );
        self.rows("topic_articles", rows)
    }

    /// The stories one monitor is *about*: what it alerted on, and what it already judged
    /// — the list a monitor row opens.
    ///
    /// Two tables answer this, and they answer different halves of the same question:
    ///
    /// * `alerts` is what the monitor decided was news (`article_id`, set on every alert
    ///   the tool protocol could tie to a stored story);
    /// * `monitor_seen` is what it has already judged, so a second check does not pay for
    ///   it twice.
    ///
    /// An article can be in either and not the other — a judged-and-dismissed story has a
    /// mark and no alert, and an alert written before the mark was recorded has the
    /// opposite — so the union is the honest set of "this monitor's stories". Reading only
    /// `monitor_seen` would show the reader the stories the monitor *rejected* and hide
    /// the ones it surfaced; reading only `alerts` would hide the rest of what it knows.
    ///
    /// The mark's key is the tool protocol's, not a column: `atlas-monitor` writes
    /// `monitor_seen(monitor_id, key)` with `key = "a<article id>"`
    /// (`atlas-monitor/src/candidates.rs`, `Candidate::key`; the Python collector used the
    /// same shape). Marks of any other shape — a judged page that never became a story —
    /// are simply not articles, and `m.key = 'a' || a.id` never matches them. That keeps
    /// this a table of *marks* rather than a foreign key, which is what it is: the writer
    /// must not have to invent an article row to remember it looked at something.
    ///
    /// `relevant`/`dupe`/`published` are [`ARTICLE_WINDOW`]'s and the order is
    /// [`Archive::articles`]', so a monitor's page reads exactly like every other list.
    /// A monitor with no story in the window, an unknown monitor id, and a non-positive
    /// limit each answer an empty list: "this monitor has nothing this window" is a
    /// sentence the page prints, not an error.
    ///
    /// Measured on the shipped archive, 72 h window (fastest of twelve runs): monitor 1
    /// ("Künstliche Intelligenz") has 22 marks of which 6 name an article, and 2 alert rows
    /// that point at a stored story; **1** of those survives [`ARTICLE_WINDOW`] (the other
    /// marked stories are `relevant = 0` — a monitor's marks are mostly the stories it
    /// judged and dismissed), in 0.35-0.48 ms. The same single row at 168 h. `EXPLAIN
    /// QUERY PLAN`: the window range read, then both tables probed by their own keys
    /// (`monitor_seen`'s primary key, `idx_alerts_article`).
    ///
    /// That number is smaller than what the monitor *row* prints, and the difference is
    /// not this statement's to close: `monitors.hits` counts **alerts** (13 for monitor 1),
    /// and 11 of those alerts are pages the archive never stored — they exist only as
    /// `alerts` rows with their own url, title and summary. A page built on this method
    /// shows the monitor's stories that are also stories of the archive; showing all 13
    /// means rendering `alerts` itself, which is a different read (and a different row
    /// type) than an [`Article`] list.
    pub fn monitor_stories(&self, monitor_id: i64, hours: i64, limit: i64) -> Vec<Article> {
        if monitor_id <= 0 || limit <= 0 {
            return Vec::new();
        }
        let since = window_start_iso(hours);
        let sql = format!(
            "SELECT {ARTICLE_COLUMNS} FROM articles a \
             LEFT JOIN sources s ON s.id = a.source_id \
             WHERE {ARTICLE_WINDOW} \
               AND (EXISTS (SELECT 1 FROM monitor_seen m \
                            WHERE m.monitor_id = ?2 AND m.key = 'a' || a.id) \
                 OR EXISTS (SELECT 1 FROM alerts al \
                            WHERE al.monitor_id = ?2 AND al.article_id = a.id)) \
             ORDER BY a.published DESC, a.id DESC LIMIT ?3"
        );
        let mut statement = match self.conn.prepare(&sql) {
            Ok(statement) => statement,
            // An archive from before the monitor half existed answers "no stories", the same
            // way [`Archive::topic_articles`] does for a missing `article_topics`; the
            // failure is recorded so a page can say which of the two it is.
            Err(error) => return self.failed("monitor_stories", &error),
        };
        let rows = statement.query_map(rusqlite::params![since, monitor_id, limit], article_row);
        self.rows("monitor_stories", rows)
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        let mut statement = match self.conn.prepare(
            "SELECT id, coalesce(name,''), coalesce(query,''), coalesce(active,0), \
             coalesce(cadence_minutes,0), coalesce(last_run,''), coalesce(hits,0) \
             FROM monitors ORDER BY active DESC, id DESC",
        ) {
            Ok(statement) => statement,
            Err(error) => return self.failed("monitors", &error),
        };
        let rows = statement.query_map([], |row| {
            Ok(Monitor {
                id: row.get(0)?,
                name: row.get(1)?,
                query: row.get(2)?,
                active: row.get::<_, i64>(3)? != 0,
                cadence: row.get(4)?,
                last_run: row.get(5)?,
                hits: row.get(6)?,
            })
        });
        self.rows("monitors", rows)
    }

    /// One `meta` value, or `None`: the agent's own key/value table, read the way every
    /// other read here is — one `query_row`, no second connection, nothing written.
    ///
    /// A missing table or a missing key is `None` rather than a sentence, because a
    /// setting the archive has never been told is not an error: the caller says what the
    /// default is (the search engine the app ships with), and a page cannot be broken by a
    /// checkout whose archive is older than the key.
    pub fn meta(&self, key: &str) -> Option<String> {
        match self.conn.query_row("SELECT v FROM meta WHERE k = ?1", [key], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(value) => Some(value).filter(|value| !value.trim().is_empty()),
            // A key the archive has never been told is `QueryReturnedNoRows`, which is an
            // answer and not a failure. A missing `meta` table is a failure, and it is
            // recorded so the status row can say the archive is older than the setting
            // rather than that the setting is empty.
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => {
                self.note("meta", &error);
                None
            }
        }
    }

    /// The briefing the agent wrote for one scope and window, or `None`.
    ///
    /// `None` is four answers this page cannot tell apart and does not need to: the table
    /// does not exist yet (an archive written before the feature), the agent has not written
    /// this scope, the row's window is a different `hours`, or the read failed. All four mean
    /// the same thing to the reader — "not written yet" — and the card offers the action that
    /// changes it. A sentence here would be the window reporting on a table it does not own.
    ///
    /// Two columns are read forgivingly, because they are the model's output and the model is
    /// not a schema:
    ///
    /// * `bullets` is JSON written by the agent; a row whose array does not parse loses its
    ///   bullets **and nothing else**. The line is the sentence a reader came for, and
    ///   refusing the whole briefing over a bracket would hide it.
    /// * every other column takes its type from the table (`i64` for the four integers,
    ///   `String` for the rest), so a `NULL` anywhere is a read that answers `None` rather
    ///   than a wrong number: the contract has no nullable column.
    pub fn briefing(&self, scope: &str, key: &str, hours: i64) -> Option<Briefing> {
        // The bullets are read as the text SQLite stores and interpreted after the row is
        // out: a `Vec<String>` is not a column type, and a closure cannot repair one entry
        // without hiding the rest of the row.
        let read = self.conn.query_row(
            "SELECT scope, key, hours, watermark, created, line, bullets, articles, model, ms \
             FROM briefings WHERE scope = ?1 AND key = ?2 AND hours = ?3",
            rusqlite::params![scope, key, hours],
            |row| {
                Ok((
                    Briefing {
                        scope: row.get(0)?,
                        key: row.get(1)?,
                        hours: row.get(2)?,
                        watermark: row.get(3)?,
                        created: row.get(4)?,
                        line: row.get(5)?,
                        bullets: Vec::new(),
                        articles: row.get(7)?,
                        model: row.get(8)?,
                        ms: row.get(9)?,
                    },
                    row.get::<_, String>(6)?,
                ))
            },
        );
        let (mut row, bullets) = match read {
            Ok(row) => row,
            // Nothing written for this scope and window is an answer (`None`), not a failure;
            // a missing `briefings` table or a column of the wrong type is a failure, and it
            // is recorded so the card can say the archive cannot be read rather than that the
            // model has not written yet.
            Err(rusqlite::Error::QueryReturnedNoRows) => return None,
            Err(error) => {
                self.note("briefing", &error);
                return None;
            }
        };
        row.bullets = read_bullets(&bullets);
        Some(row)
    }

    /// The newest article id in the archive, or 0.
    ///
    /// This is the freshness mark a briefing is compared against: an id is monotonic in
    /// arrival, so `newest_id() > briefing.watermark` means "stories landed after the model
    /// looked" — which is exactly the sentence the card prints. A stamp would be the
    /// publisher's clock, which can go backwards.
    ///
    /// 0 for an archive with no `articles` table: nothing has arrived, so nothing is newer
    /// than a briefing that was written over the same nothing.
    pub fn newest_id(&self) -> i64 {
        match self
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM articles", [], |row| {
                row.get::<_, i64>(0)
            }) {
            Ok(id) => id,
            // 0 is the honest answer for a file with no `articles` table (nothing has
            // arrived), and it is recorded as a failed read so a card comparing watermarks
            // can say the file cannot be read rather than that nothing is new.
            Err(error) => {
                self.note("newest_id", &error);
                0
            }
        }
    }
}

/// The bullets column, as a list of strings; a row that does not parse as an array of
/// strings answers an empty list.
///
/// Written as one function because it is the only place this crate interprets stored JSON
/// that a *model* produced: `serde_json` would happily hand back numbers or objects, and a
/// bullet that is not text cannot be laid out on a line.
fn read_bullets(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| {
            Some(
                value
                    .as_array()?
                    .iter()
                    .filter_map(|item| item.as_str())
                    .filter(|bullet| !bullet.trim().is_empty())
                    .map(|bullet| bullet.trim().to_string())
                    .collect::<Vec<String>>(),
            )
        })
        .unwrap_or_default()
}

/// One [`Article`] from a row of [`ARTICLE_COLUMNS`], in its order.
fn article_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Article> {
    Ok(Article {
        id: row.get(0)?,
        title: row.get(1)?,
        source: row.get(2)?,
        published: row.get(3)?,
        summary: row.get(4)?,
        content: row.get(5)?,
        country: row.get(6)?,
        url: row.get(7)?,
        full_text: row.get::<_, i64>(8)? != 0,
    })
}

/// The article statement, with or without the search predicate.
///
/// Two shapes and two parameter lists rather than one shape with a `'%%'` sentinel:
/// an empty box then costs no `LIKE` evaluation at all, and a reader who types `%`
/// gets a search for a percent sign instead of brushing against a magic string.
fn article_sql(searching: bool) -> String {
    format!(
        "SELECT {ARTICLE_COLUMNS} FROM articles a LEFT JOIN sources s ON s.id = a.source_id \
         WHERE {ARTICLE_WINDOW}{} ORDER BY a.published DESC, a.id DESC LIMIT {}",
        if searching { ARTICLE_SEARCH } else { "" },
        if searching { "?3" } else { "?2" }
    )
}

/// `%text%` with the reader's own `%`, `_` and `\` escaped, for
/// `LIKE ? ESCAPE '\'`.
///
/// A search for `100%` is a search for that text: bound raw it becomes
/// `LIKE '%100%%'`, which matches every row that merely starts with `100`, and a
/// search for `_` becomes "any one character" — the whole window.
fn like_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    out.push('%');
    for ch in text.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// The window's lower bound as an ISO stamp, so the comparison stays a string
/// compare on the format the archive stores (UTC, `Z`, fixed width).
fn window_start_iso(hours: i64) -> String {
    window_start_iso_at(now_secs(), hours)
}

/// [`window_start_iso`] with "now" given, so a test can pin the arithmetic.
///
/// Seconds, not calendar days: 72 h is 259 200 s on every day of the year, so a
/// bound never snaps to midnight and never drifts across a day, month, year or leap
/// day (`the_window_bound_is_exactly_the_hours_asked_for_across_every_boundary`).
/// The archive stores UTC, so there is no zone and no DST rule in this decision.
fn window_start_iso_at(now: i64, hours: i64) -> String {
    iso_from_epoch(now - hours.max(0) * 3_600)
}

/// Seconds since the Unix epoch as `2026-11-21T10:25:12Z` — the archive's format.
fn iso_from_epoch(secs: i64) -> String {
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// Seconds since the Unix epoch, UTC.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since the Unix epoch to a civil date, by Howard Hinnant's algorithm: no
/// dependency, no time zone, no leap-second table.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// The tests are `src/data/tests.rs`, found by the library exactly as a module's children
// always are. `src/tests.rs` is a symlink to that same file, and it exists for the
// `#[path]`-included copies: rustc resolves a `#[path]`-included file's children against
// the directory *in the path string*, so `#[path = "../src/data.rs"] mod data;` (three
// integration tests until they move to `use atlas_desktop::data;`, which the new `[lib]`
// target now allows) asks for `src/tests.rs`. A pointer, not a copy: there is one file to
// keep right. The include site that goes through `tests/support/data.rs` resolves here to
// the deliberately empty `tests/support/tests.rs` instead, because those two tests stub
// no `crate::assets` and these tests would not compile there.
#[cfg(test)]
mod tests;
