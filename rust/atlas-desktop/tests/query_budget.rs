//! What one read of the archive costs, and what it may not cost.
//!
//! The complaint is "the window feels slow". Speed is a design property here
//! (`docs/RUST-STYLE.md`), but a property nobody measures drifts: an index gets
//! retired, a `LIMIT` is dropped while adding a column, a window predicate is wrapped
//! in `coalesce` and stops being a range read. None of those change an answer, so no
//! test of behaviour notices them — only a test of cost does. This file is that test
//! for `atlas_desktop::data`, the one module between the window and `data/atlas.db`.
//!
//! # What is measured, and against what
//!
//! The nine paths one snapshot of the window touches, in `app::Atlas::reload`'s own
//! order and with its own limits: `counts`, `pins`, `countries`, `articles`,
//! `entities`, `edges`, `topics`, `monitors`, and the link/thread read (the thread
//! list, each thread's member stories, and the link rows among them — see
//! [`read_threads`], and the note below on why that one is not an `Archive` method).
//!
//! Every measurement is against the archive in this checkout, opened exactly the way
//! the window opens it — `data::Archive::open`, i.e. `SQLITE_OPEN_READ_ONLY` plus
//! `PRAGMA query_only` — never a copy, never a temp file, and never a write. Eight of
//! the nine are the library's own methods, called through `atlas_desktop::data`, so
//! what is timed is the crate's statement *and* its row decoding, not a copy of
//! either. A checkout without `data/atlas.db` prints one sentence and passes: a bare
//! clone has nothing to measure, and a measurement is not a reason for the suite to be
//! red on somebody else's machine.
//!
//! The archive is **alive**: the collector writes to `data/atlas.db` while the window
//! reads it, and it grew under this file between two runs (2 318 rows a pass, then 2 986).
//! Every ceiling here is therefore three times the *largest* reading seen, not the
//! smallest, and a failure that says the archive has grown is a signal to re-measure
//! rather than a bug — the reads are `LIMIT`ed, but the scans they walk are not.
//!
//! # Why the verdict is on the minimum, and why the budgets are three times it
//!
//! A query's own cost is the floor of its distribution: another process on this
//! machine can only add time to a run, never take it away, so the fastest of a dozen
//! runs — twice over, see [`fastest`] — is the closest thing to what the path costs.
//! Budgets are then set at roughly three times the measured minimum: that is the room a
//! slower machine needs to still pass, while the failure this file exists for — a lost
//! index, a dropped `LIMIT`, a predicate that became a full scan — costs an order of
//! magnitude, not ten percent. The measured number sits next to every budget in
//! [`BUDGETS`], so a number and its reason cannot drift apart.
//!
//! A budget is also compared against *this machine at this moment*, not against the
//! machine the number came from: [`load`] times a canary scan on the same archive and
//! stretches every ceiling by the result (never below 1, never above
//! [`CANARY_MAX_SCALE`]). Without that, a neighbouring build multiplies a sub-millisecond
//! read by ten and the test reports a lost index that is not there.
//!
//! This file deliberately does not restate what `tests/perf.rs` already pins; it adds
//! the two things a per-path budget cannot see: the **sum** (a snapshot is nine reads
//! plus a thread read, so a path that doubles while another halves still makes the
//! window feel slower) and **growth on repetition** (a cache keyed by query or needle
//! that nobody evicts answers the first keystroke fast and the hundredth slowly).
//!
//! # The ninth read, which the data layer does not have yet
//!
//! `data::Archive` has no link/thread read at all today — eight methods, and none of
//! them touches `threads`, `article_threads` or `article_links`. So [`read_threads`] is
//! written here against the archive's own tables (the schema `atlas-store` declares)
//! and timed on a second read-only connection opened with the same flags and the same
//! `query_only` pragma. That is the cost a `thread(&self, thread_id, limit)` method
//! would pay; when the method exists, this reading should call it and the three
//! statements below should go away. The budget is set here so the day it lands there is
//! already a ceiling waiting for it.
//!
//! # Read-only, twice over
//!
//! Nothing here writes, creates or copies the archive. `Archive::open` is the app's own
//! function; [`thread_conn`] repeats its flags for the one connection that does not
//! come from it, and `PRAGMA query_only` is set on both. A WAL archive gets its `-shm`
//! touched by any read (the app's read does the same) and its `-wal` stays as the
//! writer left it.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atlas_desktop::assets;
use atlas_desktop::data::Archive;
use rusqlite::{Connection, OpenFlags};

/// The width the window opens on (`app::Atlas::new` sets `hours: 72`; the others are
/// 24 and 168, so 72 is neither the cheapest nor the narrowest).
const HOURS: i64 = 72;

/// The limits `app::Atlas::reload` passes, and the ones this file must therefore
/// measure. [`the_limits_measured_are_the_ones_the_window_passes`] reads `app.rs` and
/// fails if these stop matching, so the numbers below always describe a real snapshot.
const PINS: i64 = 400;
const COUNTRIES: i64 = 400;
const ARTICLES: i64 = 400;
const ENTITIES: i64 = 120;
const EDGES: i64 = 900;

/// The thread view's page: the threads list, each thread's member stories, and the
/// link rows among the members collected. Bounded like every other read here.
const THREADS: i64 = 40;
const THREAD_ARTICLES: i64 = 20;
const LINKS: i64 = 200;

/// Timed runs per path, after [`WARMUP`] discarded ones.
const RUNS: usize = 12;
/// The first runs pay the page cache and the schema read, and would report that
/// instead of the path.
const WARMUP: usize = 2;
/// Runs for the deliberately broken probes: they cost tens of milliseconds each, and
/// a proof that the budget bites does not need forty samples of it.
const PROBE_RUNS: usize = 3;

/// The whole snapshot — all nine paths, one after the other, as `reload` runs them.
///
/// Measured minimum 3.61 ms on the window this file was written on and 5.26 ms on the
/// same archive a day later, when the collector had added stories to the window (2 986
/// rows a pass instead of 2 318); budget 16 ms — three times the larger reading. It fails
/// a snapshot that lost an index, because one unindexed search of this archive costs
/// 42-198 ms ([`the_budgets_bite_an_unindexed_or_unbounded_read`] measures it). It was
/// 17 ms while `pins` carried its correlated subquery.
const SNAPSHOT_BUDGET_MS: f64 = 16.0;

/// Repeated snapshots for the growth check: a cache that silently grows shows up in
/// the sixth pass, not the second.
const PASSES: usize = 6;
/// What a repeat may cost relative to the first pass: noise, not growth. A per-read
/// cache keyed by needle grows by `~n` statements, which is far past this.
const REPEAT_FACTOR: f64 = 3.0;
/// The floor under [`REPEAT_FACTOR`]: below a millisecond the clock's resolution is
/// most of what is being compared.
const REPEAT_SLACK_MS: f64 = 1.0;

/// A canary: the same full-table walk on every run, measured in the same moment as the
/// budgets, so a busy machine is read as a busy machine.
///
/// The ceilings in [`BUDGETS`] were measured on an idle machine. This checkout is not
/// always idle — another agent's build owns every core for a minute at a time, and that
/// multiplies a sub-millisecond read by ten (measured: `edges` 3.8 ms idle, 25 ms median
/// while a neighbour compiled, 48 ms at worst). A fixed ceiling cannot tell that apart
/// from a lost index, and a ceiling loose enough to tolerate it would not catch one. So
/// each budget is scaled by how far this scan has drifted from its own baseline: it is a
/// full walk of `articles` with no index to help it, so it drifts with the machine in the
/// same direction the measured reads do.
///
/// It cannot hide a regression: the canary is not one of the nine paths, so a query that
/// loses its index inflates its own reading and leaves the canary where it was.
const CANARY_SQL: &str = "SELECT count(*) FROM articles WHERE content LIKE '%e%'";

/// The canary's cost on an idle machine, in milliseconds: the divisor that turns a
/// loaded reading back into a budget. Measured 0.77-0.90 ms on this archive (1 435
/// bodies) with the machine quiet; the divisor sits at the bottom of that range, because
/// the scale may only ever go up.
const CANARY_BASELINE_MS: f64 = 0.88;

/// How far the canary may stretch a budget before the numbers stop meaning anything.
/// Past four times its baseline this machine is too busy to measure anything, and a test
/// that kept stretching would be hiding the regression it exists for.
const CANARY_MAX_SCALE: f64 = 4.0;

/// One path's ceiling, in milliseconds, and why it has that value.
struct Budget {
    path: &'static str,
    ms: f64,
    why: &'static str,
}

/// The ceilings: roughly three times the minimum each path measured on this archive
/// (`data/atlas.db`, 1 435 articles of which 269 are relevant, 252 inside the 72 h
/// window, 33 pins, 4 539 entities, 7 338 edges, 212 links, 28 threads), measured on
/// 2026-11-22 in the debug profile `cargo test` uses, with the machine quiet (the canary
/// reading its 0.88 ms baseline). Sub-tenth-of-a-millisecond paths get 0.6 ms whatever
/// they measured: a ceiling below that measures the clock, not the query.
///
/// `pins` is the row that has moved most, and it is the reason to keep this file: at
/// 0.46 ms it grew a correlated `newest` subquery that re-scanned the window once per
/// group and cost 4.5-5.6 ms — caught here, not by any behaviour test. It is now a
/// window-function pass (`row_number() OVER (PARTITION BY label, iso …)`) inside one CTE,
/// which reads the window once: 1.0 ms measured, and the ceiling came back down to 3 ms
/// with it. A budget that is only ever raised is not a budget.
const BUDGETS: &[Budget] = &[
    Budget {
        path: "counts",
        ms: 0.6,
        why: "eight counts in one statement, per snapshot; measured 0.033-0.067 ms",
    },
    Budget {
        path: "pins(72h, 400)",
        ms: 3.0,
        why: "the map's markers: one window pass (row_number/count/avg OVER the label) over \
              the window, 33 pins; measured 1.0 ms, after the correlated `newest` subquery \
              that cost 4.5-5.6 ms was replaced",
    },
    Budget {
        path: "countries(72h, 400)",
        ms: 1.5,
        why: "the country rollup under the map, every country a story names: 56 rows from a \
              `UNION` of the primary places and `article_countries` grouped by code; measured \
              0.62 ms, against 0.10-0.19 ms for the primary column alone",
    },
    Budget {
        path: "articles(72h, 400)",
        ms: 3.5,
        why: "the list: 252 rows with their bodies, through idx_articles_window; measured 0.90-1.10 ms",
    },
    Budget {
        path: "country_stories(72h, first iso, 400)",
        ms: 1.6,
        why: "a country chip's list: the 105 rows the chip counted, by `iso` — the read \
              `app::stories_for` runs the moment the chip is clicked, *instead of* the list, \
              not beside it, so it is budgeted here rather than added to the snapshot sum. \
              Measured 0.535-0.599 ms through idx on this archive, 2026-11-22; the ceiling is \
              three times the minimum, as every row above is"
    },
    Budget {
        path: "entities(72h, 120)",
        ms: 1.5,
        why: "the graph's 120 nodes by last_seen; measured 0.12 ms on the window this file was \
              written on and 0.41 ms after the collector added a day of stories, so the \
              ceiling is three times the larger reading",
    },
    Budget {
        path: "edges(120 ids, 900)",
        ms: 5.0,
        why: "the lines among those nodes: a 240-parameter read, re-prepared per call; measured 0.62 ms warm, 3.37 ms with the pages cold",
    },
    Budget {
        path: "topics",
        ms: 0.6,
        why: "the topic list, an unordered read of a three-row table; measured 0.013 ms",
    },
    Budget {
        path: "monitors",
        ms: 0.6,
        why: "the monitor list, the same shape; measured 0.012 ms",
    },
    Budget {
        path: "threads(40)",
        ms: 2.5,
        why: "the thread list, 28 threads' stories by idx_article_threads_thread, and the links among them; measured 0.52-0.78 ms",
    },
];

/// A timing is a measurement of the machine, so two tables are never taken at once:
/// tests in one binary run on several threads, and the fixture tests in this same
/// binary would otherwise be inside these numbers.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// One path's samples.
struct Measured {
    path: &'static str,
    min: Duration,
    median: Duration,
    max: Duration,
    /// What the answer carried. A fast number from an empty answer measured nothing.
    rows: usize,
}

impl Measured {
    fn ms(spent: Duration) -> f64 {
        spent.as_secs_f64() * 1000.0
    }

    fn min_ms(&self) -> f64 {
        Self::ms(self.min)
    }

    fn max_ms(&self) -> f64 {
        Self::ms(self.max)
    }
}

/// One path's ceiling, or a panic: every measured path has one, or the numbers below
/// would be printed and never judged.
fn budget(path: &str) -> &'static Budget {
    BUDGETS
        .iter()
        .find(|budget| budget.path == path)
        .unwrap_or_else(|| panic!("{path} is measured without a budget"))
}

/// Time one path: [`WARMUP`] runs thrown away, then [`RUNS`] samples. `run` answers
/// what the path returned, so the row count is measured with the query.
fn measure(path: &'static str, run: impl FnMut() -> usize) -> Measured {
    measure_n(path, RUNS, WARMUP, run)
}

/// [`measure`] with the sample count given, for a probe too slow to run forty times.
fn measure_n(
    path: &'static str,
    runs: usize,
    warmup: usize,
    mut run: impl FnMut() -> usize,
) -> Measured {
    for _ in 0..warmup {
        run();
    }
    let mut times: Vec<Duration> = Vec::with_capacity(runs);
    let mut rows = 0;
    for _ in 0..runs {
        let started = Instant::now();
        rows = run();
        times.push(started.elapsed());
    }
    times.sort();
    Measured {
        path,
        min: times[0],
        median: times[times.len() / 2],
        max: *times.last().unwrap(),
        rows,
    }
}

/// The faster of two rounds, taken path by path: the lower minimum, with the samples of
/// whichever round won.
///
/// Two rounds are 24 samples instead of 12 for the price of one more pass over a 7.7 MB
/// file, and they are the cheapest defence against the one thing a budget cannot control:
/// a neighbour's build taking every core for a second. A single round's minimum can land
/// inside such a stall from end to end; the minimum of two rounds almost never does, and
/// a regression has to beat *both* rounds to hide.
fn fastest(first: Vec<Measured>, second: Vec<Measured>) -> Vec<Measured> {
    first
        .into_iter()
        .zip(second)
        .map(|(first, second)| if second.min < first.min { second } else { first })
        .collect()
}

/// How much slower this machine is right now than the idle machine the budgets were
/// measured on, as a factor never below 1 and never above [`CANARY_MAX_SCALE`].
fn load(conn: &Connection) -> f64 {
    let measured = measure("canary", || {
        conn.query_row(CANARY_SQL, [], |row| row.get::<_, i64>(0))
            .map(|found| found as usize)
            .unwrap_or(0)
    });
    assert!(
        measured.rows > 0,
        "the canary matched nothing, so it timed an empty scan"
    );
    eprintln!(
        "canary: {:.3} ms for a full scan of articles (idle baseline {:.3} ms)",
        measured.min_ms(),
        CANARY_BASELINE_MS
    );
    (measured.min_ms() / CANARY_BASELINE_MS).clamp(1.0, CANARY_MAX_SCALE)
}

/// The table, and then each row against its ceiling with the reason beside it.
///
/// `scale` is [`load`]: `1.0` on an idle machine, higher while a build next door owns
/// the cores. It multiplies the ceilings and nothing else, and it is printed, so a
/// failure reads as "3 ms of work on a machine running 1.4 times slower than the one the
/// budget came from" instead of as a mystery.
fn judge(rows: &[Measured], scale: f64) {
    eprintln!();
    eprintln!(
        "this machine reads {} times the idle baseline",
        format!("{scale:.2}")
    );
    eprintln!(
        "{:<22} {:>9} {:>9} {:>9} {:>7} {:>9}",
        "path", "min", "median", "max", "rows", "budget"
    );
    for row in rows {
        eprintln!(
            "{:<22} {:>7.3}ms {:>7.3}ms {:>7.3}ms {:>7} {:>7.1}ms",
            row.path,
            row.min_ms(),
            Measured::ms(row.median),
            row.max_ms(),
            row.rows,
            budget(row.path).ms * scale
        );
    }
    eprintln!();
    for row in rows {
        eprintln!("{:<22} {}", row.path, budget(row.path).why);
    }
    eprintln!();
    for row in rows {
        let ceiling = budget(row.path);
        assert!(
            row.min_ms() <= ceiling.ms * scale,
            "{}: its fastest run was {:.3} ms, over the {:.1} ms budget ({:.1} ms on this \
             machine, {scale:.2} times the idle baseline) — {}",
            row.path,
            row.min_ms(),
            ceiling.ms,
            ceiling.ms * scale,
            ceiling.why
        );
    }
}

/// The window's archive, opened the way the window opens it, or a sentence saying why
/// there is nothing to measure. The caller returns; it does not fail.
fn open() -> Option<Archive> {
    let path = assets::archive();
    if !path.is_file() {
        eprintln!(
            "skipped: no archive at {} — start ATLAS once so it is created",
            path.display()
        );
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some(archive),
        Err(sentence) => {
            eprintln!("skipped: {sentence}");
            None
        }
    }
}

/// A window that has aged empty measures nothing: an empty answer is fast for a reason
/// that has nothing to do with the query. Say so instead of timing it.
fn window_is_empty(archive: &Archive) -> bool {
    if archive.articles(HOURS, 1, "").is_empty() {
        eprintln!(
            "skipped: the {HOURS}h window holds no relevant story (the archive's newest is \
             older than that) — the windowed paths have nothing to read"
        );
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// the ninth read: links and threads, which the archive has tables for and the
// desktop's data layer has no method for yet
// ---------------------------------------------------------------------------

/// The thread list, newest first, as the thread page orders it.
const THREAD_LIST: &str = "SELECT t.id, coalesce(t.title,''), coalesce(t.size,0), \
     coalesce(t.updated,'') FROM threads t ORDER BY coalesce(t.updated,'') DESC, t.id DESC \
     LIMIT ?1";

/// One thread's stories, through the index `atlas-store` added for this question
/// (`idx_article_threads_thread`; its own test pins the plan).
const THREAD_MEMBERS: &str = "SELECT ar.id, coalesce(ar.title,''), coalesce(ar.published,'') \
     FROM article_threads at2 JOIN articles ar ON ar.id = at2.article_id \
     WHERE at2.thread_id = ?1 ORDER BY ar.published DESC LIMIT ?2";

/// A second read-only connection for the one path `Archive` has no method for: the
/// same flags and the same pragma `Archive::open` uses, so the cost is the window's.
fn thread_conn(path: &Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    conn.pragma_update(None, "query_only", true).ok()?;
    Some(conn)
}

/// The link/thread read: the thread list, each thread's member stories, and the link
/// rows among the members collected — the three statements a thread page needs.
/// Answers how many rows it read, so an empty answer cannot pass as a fast one.
///
/// The member ids are bound, never interpolated (the same rule `Archive::edges`
/// follows), and both the thread list and each thread's stories are `LIMIT`ed: a
/// thread page cannot grow with the archive.
fn read_threads(conn: &Connection, threads: i64, per_thread: i64, links: i64) -> usize {
    let mut rows = 0;

    let mut list = match conn.prepare(THREAD_LIST) {
        Ok(statement) => statement,
        Err(_) => return 0,
    };
    let thread_ids: Vec<i64> = list
        .query_map([threads], |row| row.get::<_, i64>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    rows += thread_ids.len();

    let mut members = match conn.prepare(THREAD_MEMBERS) {
        Ok(statement) => statement,
        Err(_) => return rows,
    };
    let mut drawn: Vec<i64> = Vec::new();
    for id in &thread_ids {
        if let Ok(found) = members.query_map(rusqlite::params![id, per_thread], |row| {
            row.get::<_, i64>(0)
        }) {
            let found: Vec<i64> = found.flatten().collect();
            rows += found.len();
            drawn.extend(found);
        }
    }
    drop(members);
    drop(list);

    // The links among the stories collected, one statement, parameters bound. A thread
    // page that read links per story would be the quadratic shape this file's proof
    // test measures 26 ms of.
    drawn.sort_unstable();
    drawn.dedup();
    drawn.truncate(links as usize);
    if drawn.is_empty() {
        return rows;
    }
    let marks = vec!["?"; drawn.len()].join(",");
    let sql = format!(
        "SELECT a, b, coalesce(score,0), coalesce(why,'') FROM article_links \
         WHERE a IN ({marks}) OR b IN ({marks}) LIMIT ?"
    );
    let mut params: Vec<i64> = Vec::with_capacity(drawn.len() * 2 + 1);
    params.extend_from_slice(&drawn);
    params.extend_from_slice(&drawn);
    params.push(links);
    if let Ok(mut statement) = conn.prepare(&sql) {
        if let Ok(found) = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, i64>(0)
        }) {
            rows += found.flatten().count();
        }
    }
    rows
}

/// One read of each of the ten paths, timed.
///
/// Nine are what `reload` runs for a page; the tenth is the read a **rollup chip** runs
/// *instead of* the list (`app::stories_for` → `Archive::country_stories`), because that is
/// what a reader waits for after a click. It is measured here so its cost is a number in this
/// file rather than an assumption, and it is deliberately not part of the snapshot's sum: the
/// window runs one or the other, never both.
fn snapshot(archive: &Archive, ids: &[i64], conn: &Connection) -> Vec<Measured> {
    let first_iso = archive
        .countries(HOURS, COUNTRIES)
        .first()
        .map(|(iso, _)| iso.clone())
        .unwrap_or_default();
    vec![
        measure("counts", || {
            let counts = archive.counts();
            counts.articles.max(0) as usize
        }),
        measure("pins(72h, 400)", || archive.pins(HOURS, PINS).len()),
        measure("countries(72h, 400)", || {
            archive.countries(HOURS, COUNTRIES).len()
        }),
        measure("articles(72h, 400)", || {
            archive.articles(HOURS, ARTICLES, "").len()
        }),
        measure("country_stories(72h, first iso, 400)", || {
            archive.country_stories(&first_iso, HOURS, ARTICLES).len()
        }),
        measure("entities(72h, 120)", || {
            archive.entities(HOURS, ENTITIES).len()
        }),
        measure("edges(120 ids, 900)", || archive.edges(ids, EDGES).len()),
        measure("topics", || archive.topics().len()),
        measure("monitors", || archive.monitors().len()),
        measure("threads(40)", || {
            read_threads(conn, THREADS, THREAD_ARTICLES, LINKS)
        }),
    ]
}

/// One snapshot pass, as `reload` runs it, answering how many rows it read.
fn one_pass(archive: &Archive, conn: &Connection) -> usize {
    let mut rows = 0;
    rows += archive.counts().articles.max(0) as usize;
    rows += archive.pins(HOURS, PINS).len();
    rows += archive.countries(HOURS, COUNTRIES).len();
    rows += archive.articles(HOURS, ARTICLES, "").len();
    let entities = archive.entities(HOURS, ENTITIES);
    rows += entities.len();
    let entity_ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    rows += archive.edges(&entity_ids, EDGES).len();
    rows += archive.topics().len();
    rows += archive.monitors().len();
    rows += read_threads(conn, THREADS, THREAD_ARTICLES, LINKS);
    rows
}

/// The archive, the ids the graph is drawn from, and the thread connection — or
/// nothing to measure.
fn prepared() -> Option<(Archive, Vec<i64>, Connection)> {
    let archive = open()?;
    if window_is_empty(&archive) {
        return None;
    }
    // The same call `reload` makes, so `edges` is asked about the set the window
    // actually asks about.
    let ids: Vec<i64> = archive
        .entities(HOURS, ENTITIES)
        .iter()
        .map(|entity| entity.id)
        .collect();
    assert!(
        !ids.is_empty(),
        "the graph has no node to draw, so the edge read was never exercised"
    );
    let conn = thread_conn(&archive.path)?;
    Some((archive, ids, conn))
}

#[test]
fn every_read_of_one_snapshot_fits_its_budget() {
    let _one_at_a_time = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some((archive, ids, conn)) = prepared() else {
        return;
    };

    eprintln!(
        "\narchive {} ({} bytes), window {HOURS}h, {RUNS} runs each, {WARMUP} warm-up runs \
         thrown away",
        archive.path.display(),
        std::fs::metadata(&archive.path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    );
    let measured = fastest(
        snapshot(&archive, &ids, &conn),
        snapshot(&archive, &ids, &conn),
    );
    judge(&measured, load(&conn));

    assert_eq!(
        measured.len(),
        BUDGETS.len(),
        "every budgeted path is measured, and nothing else is"
    );

    // The answers are real, or the numbers above describe nothing.
    for row in &measured {
        if row.path == "countries(72h, 400)" {
            // A window whose stories carry no `iso` has no country to roll up: that is
            // the archive's content talking, not the query's cost.
            continue;
        }
        assert!(
            row.rows > 0,
            "{} answered nothing at all, so its time is the time of an empty answer",
            row.path
        );
    }

    // The lists are bounded by the limits the window passes.
    let list = measured
        .iter()
        .find(|row| row.path == "articles(72h, 400)")
        .expect("the article list is measured");
    assert!(
        list.rows as i64 <= ARTICLES,
        "the list asked for {ARTICLES} rows and answered {}",
        list.rows
    );
    let lines = measured
        .iter()
        .find(|row| row.path == "edges(120 ids, 900)")
        .expect("the edges are measured");
    assert!(
        lines.rows as i64 <= EDGES,
        "the graph asked for {EDGES} lines and answered {}",
        lines.rows
    );
}

#[test]
fn the_whole_snapshot_fits_one_budget() {
    let _one_at_a_time = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some((archive, _, conn)) = prepared() else {
        return;
    };

    let total = measure("whole snapshot", || one_pass(&archive, &conn));
    assert!(
        total.rows > 0,
        "a snapshot that read nothing was not a snapshot"
    );
    // The same load factor the per-path budgets use: a busy machine gets the same
    // allowance here, and no more.
    let scale = load(&conn);
    eprintln!(
        "\nwhole snapshot: min {:.3} ms  median {:.3} ms  max {:.3} ms  ({} rows), \
         budget {:.1} ms ({SNAPSHOT_BUDGET_MS:.1} ms × {scale:.2})",
        total.min_ms(),
        Measured::ms(total.median),
        total.max_ms(),
        total.rows,
        SNAPSHOT_BUDGET_MS * scale
    );
    assert!(
        total.min_ms() <= SNAPSHOT_BUDGET_MS * scale,
        "one snapshot reads the archive in {:.3} ms at best, over the {:.1} ms budget \
         ({:.1} ms on a machine reading {scale:.2} times the idle baseline) — the window pays \
         this per reload, and a person sees it as lag",
        total.min_ms(),
        SNAPSHOT_BUDGET_MS,
        SNAPSHOT_BUDGET_MS * scale
    );
}

#[test]
fn a_repeated_snapshot_does_not_grow() {
    let _one_at_a_time = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some((archive, ids, conn)) = prepared() else {
        return;
    };

    // A cache that grows with every read is visible two ways, and this file checks both,
    // because only the second one is stable on a machine that is also compiling
    // something: the work a pass does, and the cost of each read across repeats.

    // 1. The work. A snapshot is a fixed set of `LIMIT`ed reads, so five passes must
    // read exactly the same rows as the first — a cache that keeps results (or appends
    // to them) answers more rows each time, and that is a bug whatever it does to the
    // clock.
    let held_before = archive.counts().articles;
    let mut counts: Vec<usize> = Vec::with_capacity(PASSES);
    let mut passes: Vec<Duration> = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let started = Instant::now();
        let rows = one_pass(&archive, &conn);
        passes.push(started.elapsed());
        assert!(rows > 0, "a pass read nothing, so it measured nothing");
        counts.push(rows);
    }
    eprintln!(
        "\n{PASSES} passes: {} ms, rows per pass {}",
        passes
            .iter()
            .map(|spent| format!("{:.2}", Measured::ms(*spent)))
            .collect::<Vec<String>>()
            .join(", "),
        counts
            .iter()
            .map(|rows| rows.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    );
    // The archive is written by the collector while this runs, so a pass may legitimately
    // read more rows than the one before: new stories land in the window. The comparison is
    // therefore against the archive's own count, taken before and after — if it moved, the
    // row counts are not this test's to judge, and it says so instead of guessing.
    let held_after = archive.counts().articles;
    if held_after != held_before {
        eprintln!(
            "row counts not judged: the collector wrote {} stories while these {} passes ran",
            held_after - held_before,
            PASSES
        );
    } else {
        assert!(
            counts.iter().all(|rows| *rows == counts[0]),
            "the passes read {counts:?} rows instead of the same number each time, and the \
             archive did not change ({held_before} stories before and after) — a repeated read \
             is answering more than it did the first time, which is the signature of a cache \
             that grows"
        );
    }

    // 2. The cost, read by read. Wall-clock for the whole pass is deliberately *not*
    // asserted: another agent's build on this machine puts a 200 ms stall into a pass at
    // random (measured: 32, 30, 11, 15, 206, 60 ms within one second), and a stall is not
    // a cache. The per-read minimum over [`RUNS`] samples is the comparison that survives
    // a busy machine: three rounds of the same nine reads, same values, same connection,
    // and each round's fastest run may not exceed the first round's by more than
    // [`REPEAT_FACTOR`] — a cache keyed by needle grows by one statement per keystroke,
    // which is an order of magnitude past that, and it grows monotonically, so the third
    // round would see it.
    let before = load(&conn);
    let rounds: Vec<Vec<Measured>> = (0..3).map(|_| snapshot(&archive, &ids, &conn)).collect();
    // A round measured under a neighbour's build is slower for a reason that has nothing
    // to do with a cache, so the allowance is the load at the start or the end, whichever
    // is larger: monotonic growth still exceeds both.
    let scale = before.max(load(&conn));
    for (index, later) in rounds.iter().enumerate().skip(1) {
        for (first, later) in rounds[0].iter().zip(later) {
            assert_eq!(first.path, later.path);
            let ceiling = first.min_ms() * REPEAT_FACTOR * scale + REPEAT_SLACK_MS;
            eprintln!(
                "  {:<22} round 1 {:>7.3} ms   round {} {:>7.3} ms   ceiling {:>7.2} ms",
                first.path,
                first.min_ms(),
                index + 1,
                later.min_ms(),
                ceiling
            );
            assert!(
                later.min_ms() <= ceiling,
                "{}: round {} of the identical read took {:.3} ms against round 1's {:.3} ms \
                 — a read gets slower as it is repeated, which is a cache that grows and \
                 nothing that says so",
                first.path,
                index + 1,
                later.min_ms(),
                first.min_ms()
            );
        }
    }
}

/// The budgets are worth having only if the failures they name fail them.
///
/// Two statements that are *wrong in the two ways this file exists for*, run against
/// the same archive on the same read-only connection, with the decode kept: if a
/// budget were loose enough for either of these to pass, it would be loose enough to
/// miss the regression it was set for.
///
/// * **a lost index** — a `LIKE '%…%'` cannot be answered by any index, so SQLite
///   reads every body in the archive once per outer row (1 435 × 1 435 is 23 s on this
///   archive). The probe walks forty titles, which is the same curve at a size a test
///   can afford: measured 16.4 ms for twenty titles and 133 ms for forty, against
///   the article list's 2.5 ms and the snapshot's 17 ms.
/// * **a dropped `LIMIT` and a dropped window** — every story in the archive with its
///   body, sorted, which is what the list costs without `WHERE … published >= ?` and
///   without `LIMIT ?` (`data::article_sql`'s two clauses). Its rows go through the
///   same conversions `article_row` makes, so this is the real decode and not a count.
///
/// The second probe is where the clock runs out of resolution: at 1 435 stories the
/// whole table costs 1.98 ms against the list's 0.75 ms, so a budget with honest
/// headroom for a slower machine cannot fail a dropped `LIMIT` on an archive this
/// size. That is why this file pins the *rows* as well as the milliseconds, and why
/// `tests/sql_safety.rs` asserts the statement's shape: below a few thousand stories
/// the clock is not the guard for a missing `LIMIT`, and nothing is gained by writing
/// a budget so tight that a build machine fails it.
#[test]
fn the_budgets_bite_an_unindexed_or_unbounded_read() {
    let _one_at_a_time = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some(archive) = open() else {
        return;
    };
    let Some(conn) = thread_conn(&archive.path) else {
        eprintln!("skipped: cannot open a second read-only connection");
        return;
    };

    /// Forty titles against every body: the cost of a search no index can serve.
    const UNINDEXED: &str = "SELECT count(*) FROM (SELECT title FROM articles ORDER BY id \
         LIMIT 40) a WHERE EXISTS (SELECT 1 FROM articles b WHERE b.content LIKE \
         '%' || substr(a.title, 1, 10) || '%')";
    /// The list with neither its window nor its limit: every story, body and all.
    const UNBOUNDED: &str = "SELECT id, coalesce(title,''), coalesce(content,'') FROM articles \
         ORDER BY published DESC";

    let unindexed = measure_n("probe: unindexed search", PROBE_RUNS, 1, || {
        let mut statement = conn.prepare(UNINDEXED).expect("the probe prepares");
        statement
            .query_row([], |row| row.get::<_, i64>(0))
            .map(|found| found as usize)
            .unwrap_or(0)
    });
    let unbounded = measure_n("probe: no window, no limit", PROBE_RUNS, 1, || {
        let mut statement = conn.prepare(UNBOUNDED).expect("the probe prepares");
        let mut rows = statement.query([]).expect("the probe runs");
        let mut seen = 0usize;
        while let Some(row) = rows.next().expect("a probe row") {
            seen += 1;
            // The two columns that cost: a title and a body, converted to `String`s as
            // `article_row` converts them.
            let _title: Option<String> = row.get(1).unwrap_or(None);
            let _body: Option<String> = row.get(2).unwrap_or(None);
        }
        seen
    });

    // The same load factor the budgets get, so the proof holds on a busy machine too:
    // the probes are compared with the ceilings *as stretched by this machine*.
    let scale = load(&conn);
    let list = budget("articles(72h, 400)");
    eprintln!(
        "\nbudgets that bite: unindexed search {:.2} ms, no window and no limit {:.2} ms \
         ({} rows), against the list's {:.1} ms and the snapshot's {:.1} ms \
         (× {scale:.2} for this machine)",
        unindexed.min_ms(),
        unbounded.min_ms(),
        unbounded.rows,
        list.ms * scale,
        SNAPSHOT_BUDGET_MS * scale
    );

    assert!(
        unindexed.rows > 0,
        "the unindexed probe matched nothing, so it did no work"
    );
    // Against the list's own budget, which is the ceiling this failure would have to
    // pass: 122-198 ms measured against 2.5 ms nominal, and still four times that at the
    // largest load factor the canary will grant.
    assert!(
        unindexed.min_ms() > list.ms * scale,
        "an unindexed search costs {:.2} ms, inside the {:.1} ms list budget — the budgets \
         cannot fail the lost index they exist for",
        unindexed.min_ms(),
        list.ms * scale
    );
    assert!(
        unbounded.rows > ARTICLES as usize,
        "the unbounded probe read {} rows, which is not more than the list's limit of \
         {ARTICLES}, so it did not demonstrate a dropped LIMIT",
        unbounded.rows
    );
}

/// The limits timed here are the limits the window passes.
///
/// A budget is a budget for a shape: `articles(72, 400)` and `articles(72, 4000)` are
/// different queries, and a limit raised in `app.rs` would make every number above
/// describe something the window no longer runs. The call sites are read from the
/// source, exactly as `tests/sql_safety.rs` reads them for its placeholder assertion.
///
/// **The list's limit is `app::LIST_ROWS` now, not a literal 400.** `reload` passes the named
/// constant — `articles(wanted.hours, LIST_ROWS, …)` and, for a chip, `stories_for(…,
/// LIST_ROWS)` — so the pin follows the name and the *value* is checked separately, below:
/// `LIST_ROWS` must still be the 400 these budgets were measured against, and a change to it
/// has to be a change to this file too.
#[test]
fn the_limits_measured_are_the_ones_the_window_passes() {
    const APP_RS: &str = include_str!("../src/app.rs");
    let expected = [
        ("hours: 72", "the window's own width"),
        ("pins(wanted.hours, 400)", "the map's markers"),
        ("countries(wanted.hours, COUNTRY_ROWS)", "the country rollup"),
        ("articles(wanted.hours, LIST_ROWS,", "the list"),
        // The chip's list: the read a rollup door opens, with the same cap as the list, so
        // the tenth probe in `snapshot` measures a shape the window really runs.
        ("stories_for(archive, door, wanted.hours, LIST_ROWS)", "a rollup chip's list"),
        ("entities(wanted.hours, 120)", "the graph's nodes"),
        ("edges(&ids, 900)", "the graph's lines"),
        ("topics()", "the topic list"),
        ("monitors()", "the monitor list"),
    ];
    for (call, what) in expected {
        assert!(
            APP_RS.contains(call),
            "src/app.rs no longer calls `{call}` ({what}) — the budgets in this file were \
             measured on that shape; re-measure and update them"
        );
    }
    // And the numbers above are those literals, not something else that happens to
    // compile. `LIST_ROWS` is the window's own constant, so this is the one line that ties
    // the two files together: it is read here, not copied.
    assert_eq!(
        atlas_desktop::app::LIST_ROWS, ARTICLES,
        "the window asks for {} rows and this file budgets a list of {ARTICLES}: the numbers \
         above describe a shape the window no longer runs",
        atlas_desktop::app::LIST_ROWS
    );
    assert_eq!(
        atlas_desktop::app::COUNTRY_ROWS, COUNTRIES,
        "the window asks for {} country rows and this file budgets {COUNTRIES}: the numbers \
         above describe a shape the window no longer runs",
        atlas_desktop::app::COUNTRY_ROWS
    );
    assert_eq!(
        (PINS, COUNTRIES, ARTICLES, ENTITIES, EDGES),
        (400, 400, 400, 120, 900)
    );
}
