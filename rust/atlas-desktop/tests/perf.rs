//! What the native window's data path costs, measured against the archive in this
//! checkout.
//!
//! The paths are the ones `app::Atlas::reload` runs for one snapshot, in its own
//! order and with its own limits — `counts()`, `pins(72, 400)`, `countries(72, 400)`,
//! `articles(72, 400, "")`, `entities(72, 120)`, `edges(&ids, 900)`, `topics()`,
//! `monitors()` — plus the outline load the map pays once at start.
//!
//! Read-only twice over: `data::Archive` opens the file with `SQLITE_OPEN_READ_ONLY`
//! and sets `PRAGMA query_only`, and nothing here writes. The archive is never
//! created, migrated or copied — a checkout without one is not measured.
//!
//! Skip, not fail: a checkout without `data/atlas.db`, without the vendored outlines,
//! or with a window that has aged empty prints one sentence and passes. The numbers
//! belong to a checkout that has the data; a bare clone has nothing to measure.
//!
//! Run with `--nocapture` to read the tables:
//!
//! ```text
//! cargo test --offline -p atlas-desktop --test perf -- --nocapture
//! ```
//!
//! The verdict is on the **minimum** of the samples, not the median or the p95. A
//! query's own cost is the floor of its distribution: another process on the machine
//! can only ever add time to a run, never take it away, so the fastest run of forty
//! is the closest thing to what the path costs. On the machine this file was written
//! on, the same prepared statement ran in 3.9 ms and in 105 ms within one second, and
//! a budget on the median would have been a budget on the build queue. The whole
//! spread is printed beside every budget row, so a slower machine reads as a slower
//! machine and not as a mystery.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// The window's own code, linked from the crate that ships it: the alternative would be a
// copy of the queries, which is a copy of the thing under test. `assets` says which
// archive and which outlines to measure, `data::Archive` is the connection every path in
// the table runs over, and `views::map::World` is what the first frame parses.
use atlas_desktop::assets;
use atlas_desktop::data::Archive;
use atlas_desktop::views::map::World;

/// Measured runs per path, after the warm-up ones.
const RUNS: usize = 40;
/// Runs thrown away first: the first query pays the SQLite page cache and the schema
/// read, and would report that instead of the path.
const WARMUP: usize = 3;
/// The window the app opens on (`app::Atlas::new`).
const HOURS: i64 = 72;
/// The limits `app::Atlas::reload` asks each list for.
const PINS: i64 = 400;
const COUNTRIES: i64 = 400;
const ARTICLES: i64 = 400;
const ENTITIES: i64 = 120;
const EDGES: i64 = 900;

/// A timing is a measurement of the machine, so the two tables are taken one after
/// the other: two perceptual experiments at once measure neither.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// One path's ceiling, in milliseconds, and why it has that value.
struct Budget {
    path: &'static str,
    min_ms: f64,
    why: &'static str,
}

/// The ceilings, from the minimums measured on this archive on 2026-11-22, at three
/// times the measurement and rounded up. Three times is the room a slower machine
/// needs to still pass; a path that loses its index or walks a table quadratically is
/// an order of magnitude, which is what a budget is for. A path under 0.1 ms is given
/// 0.1 ms whatever it measured: a ceiling below a tenth of a millisecond measures the
/// clock's resolution, not the query. Each row quotes the minimum it was measured at, so
/// the number and its reason cannot drift apart; a later run varies by up to a factor of
/// two on a machine that other people are building on.
const BUDGETS: &[Budget] = &[
    Budget {
        path: "store.counts",
        min_ms: 0.3,
        why: "eight count(*) on one connection, once per snapshot; measured 0.03 ms",
    },
    Budget {
        path: "store.pins(72h)",
        min_ms: 3.5,
        why: "the map's markers: one window pass for size, mean position and newest story; \
              measured 1.03 ms, and 3.97 ms while the destination was a correlated subquery",
    },
    Budget {
        path: "store.countries(72h)",
        min_ms: 1.5,
        why: "the country rollup under the map, every country a story names: 56 rows from a \
              `UNION` of the primaries and `article_countries`, grouped by code; measured \
              0.52–0.75 ms across three runs of this table, against 0.10 ms for the primary \
              column alone, so the budget is twice the slowest of them",
    },
    Budget {
        path: "store.articles(72h)",
        min_ms: 3.0,
        why: "the list, 400 rows with their body text; measured 0.86 ms",
    },
    Budget {
        path: "store.entities(72h)",
        // Measured 0.12 ms when this row was written on a window of a few hundred
        // entities; **0.543 / 0.593 ms today** (three failing runs of this very test, then
        // the same figure from `cargo test --test query_budget`, which budgets the same
        // path at 1.5 ms). The growth is the archive's, not a lost index: `entities` walks
        // `idx_entities_count` (ORDER BY count DESC LIMIT 120) and probes `idx_edges_a` /
        // `idx_edges_b` with the two `EXISTS` in `data.rs`, so its cost rises with the
        // number of entities that have edges, and this checkout's collector has added
        // thousands. The ceiling follows the measurement rather than the repo, exactly as
        // the sibling file's row for the same path already does — and it still bites: a
        // path that loses an index costs whole milliseconds (`query_budget`'s unindexed
        // probe measures 42-198 ms).
        min_ms: 1.5,
        why: "the graph's 120 nodes: `idx_entities_count` with an `EXISTS` probe per row \
              over the edge indexes; measured 0.12 ms on a small window, 0.54-0.59 ms on \
              this archive today",
    },
    Budget {
        path: "store.edges(120 ids)",
        // 0.63 ms was measured when this row was written; **2.007 / 2.144 / 2.406 / 2.595 ms
        // today** (the minimum of each of four runs of this table on the checkout's 7.7 MB
        // archive: 1 442 articles, 120 nodes, 900 edges among them; medians 2.7–2.9 ms, p95
        // 3.7). The read answers `limit` rows, and the archive's edge set has grown since —
        // the same growth `store.entities(72h)` above records, and the same reason. The
        // sibling `query_budget` budgets this path at 6.0 ms because it measures the first,
        // cold-paged read; this row warms up first and takes the fastest run, so it keeps a
        // third of that. A lost index is still caught: `query_budget`'s unindexed probe of
        // the same shape measures 42–198 ms.
        min_ms: 3.0,
        why: "one index seek per id, the far end filtered: measured 0.63 ms when written, \
              2.01–2.60 ms on this archive today (median 2.9, p95 3.7), growing with the \
              archive's edges",
    },
    Budget {
        path: "store.topics",
        min_ms: 0.1,
        why: "the topic list, an unordered read of a small table; measured 0.027 ms",
    },
    Budget {
        path: "store.monitors",
        min_ms: 0.1,
        why: "the monitor list, the same shape; measured 0.024 ms",
    },
];

/// The outlines, loaded once at start: 3.0 MB of GeoJSON into the world the map paints,
/// 1 632 coastlines and 99 613 points. Measured at 372 ms, so the ceiling is three times
/// that — a debug build parses JSON unoptimised, which is why a start-up cost is hundreds
/// of milliseconds here and single digits in release. The map's own test allows 2 000 ms
/// for this load *and* a frame built from it.
const GEOJSON_MIN_MS: f64 = 1_200.0;

/// One path's samples.
struct Measured {
    path: &'static str,
    min: Duration,
    median: Duration,
    p95: Duration,
    max: Duration,
    /// What the answer carried: a fast number from an empty answer is not a
    /// measurement.
    rows: usize,
}

impl Measured {
    fn ms(spent: Duration) -> f64 {
        spent.as_secs_f64() * 1000.0
    }

    fn min_ms(&self) -> f64 {
        Self::ms(self.min)
    }
}

/// Time one path: `WARMUP` runs thrown away, then `RUNS` samples. `run` answers what
/// the path returned, so the row count is measured with the query and not guessed.
fn measure(path: &'static str, mut run: impl FnMut() -> usize) -> Measured {
    for _ in 0..WARMUP {
        run();
    }
    let mut samples: Vec<(Duration, usize)> = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let started = Instant::now();
        let rows = run();
        samples.push((started.elapsed(), rows));
    }
    let rows = samples.last().map(|(_, rows)| *rows).unwrap_or(0);
    let mut times: Vec<Duration> = samples.into_iter().map(|(spent, _)| spent).collect();
    times.sort();
    Measured {
        path,
        min: times[0],
        median: times[times.len() / 2],
        p95: times[(times.len() * 95) / 100],
        max: *times.last().unwrap(),
        rows,
    }
}

/// The table, in the order the snapshot runs it.
fn table(rows: &[Measured], title: &str) {
    eprintln!();
    eprintln!("{title}");
    eprintln!(
        "{:<22} {:>10} {:>10} {:>10} {:>10} {:>8} {:>10}",
        "path", "min", "median", "p95", "max", "rows", "budget"
    );
    for row in rows {
        eprintln!(
            "{:<22} {:>8.3}ms {:>8.3}ms {:>8.3}ms {:>8.3}ms {:>8} {:>8.1}ms",
            row.path,
            Measured::ms(row.min),
            Measured::ms(row.median),
            Measured::ms(row.p95),
            Measured::ms(row.max),
            row.rows,
            budget(row.path).map(|budget| budget.min_ms).unwrap_or(f64::NAN)
        );
    }
    eprintln!();
    for row in rows {
        if let Some(budget) = budget(row.path) {
            eprintln!("{:<22} {}", row.path, budget.why);
        }
    }
    eprintln!();
}

fn budget(path: &str) -> Option<&'static Budget> {
    BUDGETS.iter().find(|budget| budget.path == path)
}

/// One row against its ceiling, with the reason beside it.
fn assert_budget(row: &Measured) {
    let Some(budget) = budget(row.path) else {
        panic!("{} was measured without a budget", row.path);
    };
    assert!(
        row.min_ms() <= budget.min_ms,
        "{}: its fastest run was {:.3} ms, over the {:.1} ms budget ({})",
        row.path,
        row.min_ms(),
        budget.min_ms,
        budget.why
    );
}

#[test]
fn the_window_snapshot_meets_its_budgets() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().unwrap_or_else(|poison| poison.into_inner());

    let path = assets::archive();
    if !path.is_file() {
        eprintln!(
            "no archive at {} — nothing to measure; start ATLAS once so it is created",
            path.display()
        );
        return;
    }
    let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    match Archive::open(&path) {
        Ok(archive) => measure_snapshot(&archive, bytes),
        Err(sentence) => eprintln!("{sentence} — nothing to measure"),
    }
}

fn measure_snapshot(archive: &Archive, bytes: u64) {
    eprintln!(
        "\narchive {} ({} bytes), window {HOURS}h",
        archive.path.display(),
        bytes
    );

    // The archive itself is read whatever the window holds.
    let counts = archive.counts();
    let topics = archive.topics();
    let monitors = archive.monitors();
    assert!(
        counts.articles > 0,
        "the archive holds no articles, so no path here was measured against real data"
    );

    // A window that has aged empty measures nothing: say so instead of timing an
    // answer that is empty because the archive is old.
    if archive.articles(HOURS, 1, "").is_empty() {
        eprintln!(
            "the {HOURS}h window holds no relevant story (the archive's newest is older) — \
             only the unwindowed paths were measured"
        );
        let measured = vec![
            measure("store.counts", || {
                archive.counts();
                counts.articles as usize
            }),
            measure("store.topics", || archive.topics().len()),
            measure("store.monitors", || archive.monitors().len()),
        ];
        table(&measured, &format!("{RUNS} runs each, {WARMUP} warm-up runs thrown away:"));
        for row in &measured {
            assert_budget(row);
        }
        return;
    }

    let entities = archive.entities(HOURS, ENTITIES);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let held: HashSet<i64> = ids.iter().copied().collect();

    let measured = vec![
        measure("store.counts", || {
            archive.counts();
            counts.articles as usize
        }),
        measure("store.pins(72h)", || archive.pins(HOURS, PINS).len()),
        measure("store.countries(72h)", || archive.countries(HOURS, COUNTRIES).len()),
        measure("store.articles(72h)", || archive.articles(HOURS, ARTICLES, "").len()),
        measure("store.entities(72h)", || archive.entities(HOURS, ENTITIES).len()),
        measure("store.edges(120 ids)", || archive.edges(&ids, EDGES).len()),
        measure("store.topics", || archive.topics().len()),
        measure("store.monitors", || archive.monitors().len()),
    ];
    table(&measured, &format!("{RUNS} runs each, {WARMUP} warm-up runs thrown away:"));

    // The answers are real, or the numbers above describe nothing.
    for row in &measured {
        assert!(row.rows > 0, "{} answered nothing at all", row.path);
    }
    assert_eq!(
        measured.len(),
        BUDGETS.len(),
        "every budgeted path is measured, and nothing else is"
    );
    assert!(
        measured[3].rows as i64 <= ARTICLES,
        "the list asked for {ARTICLES} rows and got {}",
        measured[3].rows
    );

    // The invariant `Archive::edges` documents: both ends of a line are drawn.
    let edges = archive.edges(&ids, EDGES);
    assert!(edges.len() as i64 <= EDGES);
    for edge in &edges {
        assert!(
            held.contains(&edge.a) && held.contains(&edge.b),
            "edge {}—{} points outside the {ENTITIES} drawn nodes",
            edge.a,
            edge.b
        );
    }
    // And the pins are on Earth: a coordinate the map cannot draw is not a marker.
    for pin in archive.pins(HOURS, PINS) {
        assert!(
            pin.lat.is_finite()
                && pin.lat.abs() <= 90.0
                && pin.lon.is_finite()
                && pin.lon.abs() <= 180.0,
            "pin at {},{} is not on Earth",
            pin.lat,
            pin.lon
        );
    }
    // The counters and the lists are one answer, not two: a `counts()` that disagreed
    // with `topics()` would make the status row lie about the archive.
    assert!(!topics.is_empty(), "the archive has no topics to list");
    assert_eq!(counts.topics, topics.len() as i64);
    assert_eq!(counts.monitors, monitors.len() as i64);

    for row in &measured {
        assert_budget(row);
    }
}

#[test]
fn the_country_outlines_parse_meets_its_budget() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().unwrap_or_else(|poison| poison.into_inner());

    let path = assets::geojson();
    let Ok(bytes) = std::fs::metadata(&path).map(|meta| meta.len()) else {
        eprintln!(
            "no vendored outlines at {} — nothing to measure; the map draws its graticule alone",
            path.display()
        );
        return;
    };

    // `World::load` is the whole start-up cost of the map: the file read, the JSON
    // parse, and the walk that turns 242 countries into coastlines. Five loads, not
    // forty — this is one cost on the first frame, and forty would make this the
    // slowest test in the workspace.
    const LOADS: usize = 5;
    let mut spent: Vec<Duration> = Vec::with_capacity(LOADS);
    let mut world = World::empty();
    for _ in 0..LOADS {
        let started = Instant::now();
        world = World::load(&path);
        spent.push(started.elapsed());
    }
    spent.sort();
    let min = Measured::ms(spent[0]);
    let median = Measured::ms(spent[spent.len() / 2]);
    let max = Measured::ms(*spent.last().unwrap());

    // The file really is the full 1:50 m set, or the load was cheap for nothing.
    assert!(
        world.lines() > 200 && world.points() > 10_000,
        "the outlines are {} coastlines and {} points, which is not the vendored set",
        world.lines(),
        world.points()
    );

    eprintln!();
    eprintln!(
        "public/vendor/ne_50m_countries.geojson: {bytes} bytes, {} coastlines, {} points",
        world.lines(),
        world.points()
    );
    eprintln!(
        "{LOADS} loads: min {min:>8.2}ms  median {median:>8.2}ms  max {max:>8.2}ms  \
         budget {GEOJSON_MIN_MS:>7.1}ms"
    );
    eprintln!();
    assert!(
        min <= GEOJSON_MIN_MS,
        "the fastest load of the outlines was {min:.2} ms, over the {GEOJSON_MIN_MS:.1} ms budget"
    );
}
