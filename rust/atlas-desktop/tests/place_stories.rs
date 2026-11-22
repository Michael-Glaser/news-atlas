//! Does every row in a list lead to its stories? This file answers it for the two lists
//! whose *data* is here: a place on the map (and a country row under it), and a monitor.
//!
//! The user's demand is one sentence — "you should always be able to click on the topics
//! and the like and then see the news; everything should be interactive". For a list row
//! that means three things have to hold at once, and none of them is visible from the
//! view: the row's own number and the page's rows must count the *same* things, the page
//! must exist for every row (never "nothing here" for a place that visibly has stories),
//! and the rows must arrive in the order the list reads in — newest first, the way
//! [`Archive::articles`] orders them.
//!
//! # What is checked, and against what
//!
//! Against the archive installed in this checkout, opened read-only through the window's
//! own [`Archive::open`] (a second connection, also read-only, answers the cross-checks by
//! SQL). Nothing writes, nothing is copied, and a checkout without `data/atlas.db` prints
//! one sentence and passes: a bare clone has nothing to answer.
//!
//! * every pin the map draws opens a list that contains the story the dot points at, and
//!   that list is as long as the pin's `count` — plus exactly the window rows with that
//!   label that carry no usable point, which a dot cannot draw and therefore cannot count;
//! * every row of the country rollup opens that country's stories, and the row's number is
//!   the length of the list (a rollup row counts a country, not a labelled place);
//! * the three places asked for by name ("United States", "China", "Washington") are
//!   counted identically by the map and by the page it opens;
//! * every monitor's page lists only stories that monitor alerted on or already judged,
//!   inside the window, newest first, and never more than the limit.
//!
//! The numbers are printed as well as asserted: a page that answers the right rows in
//! 40 ms is not interactive, so the cost is part of "it works". Measured on this archive
//! (72 h window, `min` of twelve runs after two warm-ups) — see the table the test prints.

use std::path::Path;
use std::time::{Duration, Instant};

use atlas_desktop::assets;
use atlas_desktop::data::{Archive, Article};
use rusqlite::{Connection, OpenFlags};

/// The window the map and the monitor page open on (`app::Atlas::new` sets 72).
const HOURS: i64 = 72;
/// The page size: the map's own pin limit, so a place is never cut off by the page.
const LIMIT: i64 = 400;
/// Timed runs, after [`WARMUP`] discarded ones.
const RUNS: usize = 12;
const WARMUP: usize = 2;

/// The archive, opened the way the window opens it, or a sentence saying why there is
/// nothing to answer.
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

/// A second connection to the same file, read-only, for the SQL the assertions use as an
/// oracle. Same flags and same pragma as [`Archive::open`]; it never writes.
fn read_only(path: &Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    conn.pragma_update(None, "query_only", true).ok()?;
    Some(conn)
}

/// The window's lower bound as the archive stores it, asked from SQLite so this file does
/// not carry a second copy of the date arithmetic.
fn window_start(conn: &Connection) -> String {
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        [format!("-{HOURS} hours")],
        |row| row.get(0),
    )
    .expect("the window bound")
}

/// The fastest of [`RUNS`] runs, in milliseconds: the floor of the distribution, which is
/// what a path costs on a machine that is also doing something else.
fn min_ms(mut run: impl FnMut() -> usize) -> f64 {
    for _ in 0..WARMUP {
        run();
    }
    let mut fastest = Duration::from_secs(3600);
    for _ in 0..RUNS {
        let started = Instant::now();
        run();
        fastest = fastest.min(started.elapsed());
    }
    fastest.as_secs_f64() * 1000.0
}

/// `published DESC, a.id DESC` — the order every list in the window reads in.
fn ordered_newest_first(stories: &[Article]) -> bool {
    stories
        .windows(2)
        .all(|pair| (&pair[0].published, pair[0].id) >= (&pair[1].published, pair[1].id))
}

/// The label the map groups by, for the oracle queries only: the same expression
/// [`atlas_desktop::data`] documents as `PLACE_LABEL`. A test that re-derived the place
/// name some other way would not be checking the map, so this is deliberately a copy of
/// the definition rather than a second opinion about it.
const LABEL: &str = "coalesce(nullif(a.place, ''), nullif(a.iso, ''), '')";

/// How many window rows this label has in all.
fn label_rows(conn: &Connection, since: &str, label: &str) -> i64 {
    conn.query_row(
        &format!(
            "SELECT count(*) FROM articles a WHERE {LABEL} = ?1 \
             AND a.relevant = 1 AND a.dupe = 0 AND a.published >= ?2"
        ),
        rusqlite::params![label, since],
        |row| row.get(0),
    )
    .expect("a count of the label's rows")
}

/// How many of those rows a dot cannot draw — no usable point.
fn label_rows_without_a_point(conn: &Connection, since: &str, label: &str) -> i64 {
    conn.query_row(
        &format!(
            "SELECT count(*) FROM articles a WHERE {LABEL} = ?1 \
             AND a.relevant = 1 AND a.dupe = 0 AND a.published >= ?2 \
             AND NOT (a.lat IS NOT NULL AND a.lon IS NOT NULL \
                      AND NOT (a.lat = 0 AND a.lon = 0) \
                      AND a.lat BETWEEN -90 AND 90 AND a.lon BETWEEN -180 AND 180)"
        ),
        rusqlite::params![label, since],
        |row| row.get(0),
    )
    .expect("a count of the label's unplottable rows")
}

/// Every place the map draws leads to its stories, and the number on the dot is the length
/// of that list, once the rows the dot cannot draw are accounted for.
#[test]
fn every_dot_opens_its_place_and_counts_the_same_stories() {
    let Some(archive) = open() else {
        return;
    };
    let Some(conn) = read_only(&archive.path) else {
        eprintln!("skipped: cannot open a second read-only connection");
        return;
    };
    let since = window_start(&conn);

    let pins = archive.pins(HOURS, 400);
    assert!(
        !pins.is_empty(),
        "the {HOURS}h window draws no pin at all, so nothing here was exercised"
    );

    let mut widest = (String::new(), 0usize, 0.0f64);
    for pin in &pins {
        let stories = archive.place_stories(&pin.place, HOURS, LIMIT);
        let ids: Vec<i64> = stories.iter().map(|story| story.id).collect();

        assert!(
            !stories.is_empty(),
            "the dot for {} opens nothing, and it is drawn from {} stories",
            pin.place,
            pin.count
        );
        assert!(
            ids.contains(&pin.article),
            "the dot for {} points at story {}, which its own list does not hold: {:?}",
            pin.place,
            pin.article,
            ids
        );
        assert!(
            ordered_newest_first(&stories),
            "the stories for {} are not newest first with the id tiebreak: {:?}",
            pin.place,
            stories
                .iter()
                .map(|story| (story.published.as_str(), story.id))
                .collect::<Vec<_>>()
        );

        // The dot counts what it can draw; the page lists what the place is. The two can
        // only differ by the rows without a usable point, and by nothing else.
        let unplottable = label_rows_without_a_point(&conn, &since, &pin.place);
        assert_eq!(
            stories.len() as i64 - pin.count,
            unplottable,
            "{}: the dot counts {} and the page lists {} ({} rows of that label have no \
             point) — a dot and the page it opens disagree about what belongs together",
            pin.place,
            pin.count,
            stories.len(),
            unplottable
        );

        // And the page is exactly the label's window rows, not a superset of neighbours.
        assert_eq!(
            stories.len() as i64,
            label_rows(&conn, &since, &pin.place),
            "{}: the page lists {} stories and the window holds {} with that label",
            pin.place,
            stories.len(),
            label_rows(&conn, &since, &pin.place)
        );

        let spent = min_ms(|| archive.place_stories(&pin.place, HOURS, LIMIT).len());
        if stories.len() > widest.1 {
            widest = (pin.place.clone(), stories.len(), spent);
        }
    }

    eprintln!(
        "\n{} dots opened, {} stories in the widest ({}: {} rows in {:.3} ms)",
        pins.len(),
        widest.1,
        widest.0,
        widest.1,
        widest.2
    );
}

/// The three places asked for by name: the map's count and the page's length are the same
/// number, and the page is fast enough to be a click rather than a wait.
#[test]
fn the_named_places_are_counted_the_same_way_by_the_map_and_the_page() {
    let Some(archive) = open() else {
        return;
    };
    let Some(conn) = read_only(&archive.path) else {
        return;
    };
    let since = window_start(&conn);
    let pins = archive.pins(HOURS, 400);

    for label in ["United States", "China", "Washington"] {
        let pin = pins
            .iter()
            .find(|pin| pin.place == label)
            .unwrap_or_else(|| panic!("the map draws no dot for {label} this window"));
        let stories = archive.place_stories(label, HOURS, LIMIT);
        let spent = min_ms(|| archive.place_stories(label, HOURS, LIMIT).len());

        eprintln!(
            "{:<15} dot {:>4}  page {:>4} rows  {:>6.3} ms  oldest {}",
            label,
            pin.count,
            stories.len(),
            spent,
            stories.last().map(|story| story.published.as_str()).unwrap_or("—")
        );

        assert_eq!(
            stories.len() as i64,
            pin.count + label_rows_without_a_point(&conn, &since, label),
            "{label}: the dot and the page count different stories"
        );
        assert_eq!(
            stories.len() as i64,
            label_rows(&conn, &since, label),
            "{label}: the page is not the label's window rows"
        );
        assert!(
            ordered_newest_first(&stories),
            "{label}: the stories are not newest first"
        );
        // A click has to feel like one: the widest place of this archive answers in a
        // millisecond or less, and this ceiling is three times the slowest reading seen
        // (measured 0.25-1.20 ms while other builds ran).
        assert!(
            spent < 8.0,
            "{label}: the page took {spent:.3} ms, which is a wait rather than a click"
        );
    }
}

/// Every row of the country rollup leads to that country's stories, and the number on the
/// row is the length of the list.
///
/// This is the read the rollup needs and a place label cannot give it: `countries` groups
/// by `iso`, so its row counts a *country* (the stories under a city included), while
/// `place_stories` answers the label the map draws — which for `USA` is empty on this
/// archive, because every United States story names its place.
#[test]
fn every_country_row_opens_its_stories() {
    let Some(archive) = open() else {
        return;
    };
    let Some(conn) = read_only(&archive.path) else {
        return;
    };
    let since = window_start(&conn);
    let rollup = archive.countries(HOURS, 14);
    assert!(
        !rollup.is_empty(),
        "the {HOURS}h window has no country, so nothing here was exercised"
    );

    for (iso, counted) in &rollup {
        let stories = archive.country_stories(iso, HOURS, LIMIT);
        let spent = min_ms(|| archive.country_stories(iso, HOURS, LIMIT).len());
        eprintln!(
            "{iso:<5} row {counted:>4}  page {:>4} rows  {spent:>6.3} ms",
            stories.len()
        );

        assert!(
            !stories.is_empty(),
            "the rollup row for {iso} says {counted} stories and opens nothing"
        );
        assert_eq!(
            stories.len() as i64,
            *counted,
            "{iso}: the row counts {counted} and the page lists {}",
            stories.len()
        );
        assert!(
            ordered_newest_first(&stories),
            "{iso}: the stories are not newest first with the id tiebreak"
        );
        assert!(
            stories
                .iter()
                .all(|story| story.published.as_str() >= since.as_str()),
            "{iso}: a story outside the {HOURS}h window was listed"
        );
        // The rollup's own expression, asked again independently: this is the number the
        // row was built from. A country is **every place a story names**, so the count is the
        // `UNION` of the stories filed under the code and the stories with a further row for
        // it — the same pairs `Archive::countries` groups (measured: CHN 164, not 89).
        let expected: i64 = conn
            .query_row(
                "SELECT count(*) FROM ( \
                     SELECT a.id AS id FROM articles a WHERE a.iso = ?1 \
                       AND a.relevant = 1 AND a.dupe = 0 AND a.published >= ?2 \
                     UNION \
                     SELECT c.article_id FROM article_countries c \
                       JOIN articles a ON a.id = c.article_id \
                     WHERE c.iso = ?1 \
                       AND a.relevant = 1 AND a.dupe = 0 AND a.published >= ?2 \
                 )",
                rusqlite::params![iso, since],
                |row| row.get(0),
            )
            .expect("a count of the country's rows");
        assert_eq!(stories.len() as i64, expected, "{iso}: not the country's window rows");
        assert!(
            spent < 10.0,
            "{iso}: the page took {spent:.3} ms, which is a wait rather than a click"
        );
    }
}

/// Every monitor the list shows leads to its stories: what it alerted on and what it
/// already judged, inside the window, newest first, and nothing else.
#[test]
fn every_monitor_opens_the_stories_it_alerts_on_or_judged() {
    let Some(archive) = open() else {
        return;
    };
    let Some(conn) = read_only(&archive.path) else {
        return;
    };
    let since = window_start(&conn);

    let monitors = archive.monitors();
    assert!(
        !monitors.is_empty(),
        "the archive has no monitor, so the monitor page was never exercised"
    );

    for monitor in &monitors {
        let stories = archive.monitor_stories(monitor.id, HOURS, LIMIT);
        let ids: Vec<i64> = stories.iter().map(|story| story.id).collect();
        let spent = min_ms(|| archive.monitor_stories(monitor.id, HOURS, LIMIT).len());

        eprintln!(
            "\nmonitor {} {:<26} {:>4} rows  {:>6.3} ms  {}",
            monitor.id,
            monitor.name,
            stories.len(),
            spent,
            stories
                .iter()
                .map(|story| story.published.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        assert!(
            stories.len() as i64 <= LIMIT,
            "monitor {} answered {} rows for a limit of {LIMIT}",
            monitor.id,
            stories.len()
        );
        // The row's own number is its alerts, and an alert is not always a story the
        // archive holds: this asserts the relationship rather than pretending the two
        // numbers are one, so a change on either side shows up here.
        let alerts: i64 = conn
            .query_row(
                "SELECT count(*) FROM alerts WHERE monitor_id = ?1",
                [monitor.id],
                |row| row.get(0),
            )
            .expect("a count of the monitor's alerts");
        assert_eq!(
            alerts, monitor.hits,
            "monitor {}: the row says {} hits and the archive holds {alerts} alerts",
            monitor.id, monitor.hits
        );
        eprintln!(
            "  row says {} hits, {alerts} alerts, {} rows under the monitor this window",
            monitor.hits,
            stories.len()
        );
        // The two halves of the union, asked for independently of the page's own query:
        // the stories this monitor fired an alert about, and the stories it merely marked
        // as judged (`monitor_seen`, which is what stops it looking at them twice).
        let mut reachable = conn
            .prepare(
                "SELECT a.id FROM alerts al JOIN articles a ON a.id = al.article_id \
                 WHERE al.monitor_id = ?1 AND a.relevant = 1 AND a.dupe = 0 \
                 AND a.published >= ?2",
            )
            .expect("the alerts' stories");
        let surfaced: Vec<i64> = reachable
            .query_map(rusqlite::params![monitor.id, since], |row| row.get(0))
            .expect("a read of the alerts' stories")
            .flatten()
            .collect();
        // **The list is the union of two things, so it is not bounded by the alert count.**
        //
        // This used to assert `stories.len() <= alerts`, which said a monitor's page can hold
        // no more rows than the alerts it fired. That is the wrong rule for what the page
        // promises: `Archive::monitor_stories` lists the stories the monitor **alerted on**
        // (`alerts.article_id`) *or* **already judged** (`monitor_seen`, the marks that stop a
        // story being looked at twice). A monitor that runs often judges far more stories than
        // it alerts on, so the union is legitimately larger — measured on this archive,
        // monitor 2 lists 8 rows from 4 alerts (the other 4 carry a mark and no alert).
        //
        // The rule to pin is therefore exact, not an inequality: every row is backed by an
        // alert or a mark (asserted below, per id), every alerted story in the window is on
        // the page, and **every marked story in the window is on the page too** — which the
        // old inequality never checked. The set the page returns must be exactly that union.
        let mut marked = conn
            .prepare(
                "SELECT a.id FROM monitor_seen m JOIN articles a ON m.key = 'a' || a.id \
                 WHERE m.monitor_id = ?1 AND a.relevant = 1 AND a.dupe = 0 \
                 AND a.published >= ?2",
            )
            .expect("the monitor's marks");
        let marked: Vec<i64> = marked
            .query_map(rusqlite::params![monitor.id, since], |row| row.get(0))
            .expect("a read of the marked stories")
            .flatten()
            .collect();
        let mut expected: Vec<i64> = marked.clone();
        expected.extend(surfaced.iter().copied());
        expected.sort_unstable();
        expected.dedup();
        eprintln!(
            "  {} alerts' stories + {} marked = {} distinct in the window, {LIMIT} asked for",
            surfaced.len(),
            marked.len(),
            expected.len()
        );
        assert!(
            stories.len() as i64 <= expected.len() as i64,
            "monitor {}: {} rows from a union of {} — a row that is backed by neither an \
             alert nor a mark",
            monitor.id,
            stories.len(),
            expected.len()
        );
        if expected.len() as i64 <= LIMIT {
            let mut listed: Vec<i64> = ids.clone();
            listed.sort_unstable();
            listed.dedup();
            assert_eq!(
                listed,
                expected,
                "monitor {}: the page holds {listed:?}, and the stories it alerted on or \
                 marked are {expected:?}",
                monitor.id
            );
        } else {
            // Over the limit the page shows the newest `LIMIT`, so the honest assertion is
            // that every row it shows is in the union (below, per id) and that it filled the
            // page: a short list would be stories the monitor saw and the page lost.
            assert_eq!(
                stories.len() as i64,
                LIMIT,
                "monitor {}: {} rows from a union of {}, which is under the limit — the page \
                 dropped stories the monitor saw",
                monitor.id,
                stories.len(),
                expected.len()
            );
        }
        assert!(
            ids.iter().all(|id| expected.contains(id)),
            "monitor {}: the page lists a story the monitor neither alerted on nor marked",
            monitor.id
        );

        for id in &surfaced {
            assert!(
                ids.contains(id),
                "monitor {} alerted on story {id}, which its page does not list",
                monitor.id
            );
        }
        assert!(
            ordered_newest_first(&stories),
            "monitor {}: not newest first with the id tiebreak",
            monitor.id
        );
        // Inside the window, by the archive's own string comparison.
        for story in &stories {
            assert!(
                story.published.as_str() >= since.as_str(),
                "monitor {} listed {} published {}, outside the {HOURS}h window (>= {since})",
                monitor.id,
                story.id,
                story.published
            );
        }
        // Every row is backed by this monitor: an alert pointing at it, or a mark naming
        // it. A row that is backed by neither would be a story the monitor never saw.
        for id in &ids {
            let backed: i64 = conn
                .query_row(
                    "SELECT (SELECT count(*) FROM monitor_seen m \
                              WHERE m.monitor_id = ?2 AND m.key = 'a' || ?1) \
                          + (SELECT count(*) FROM alerts al \
                              WHERE al.monitor_id = ?2 AND al.article_id = ?1)",
                    rusqlite::params![id, monitor.id],
                    |row| row.get(0),
                )
                .expect("a count of the marks behind a story");
            assert!(
                backed > 0,
                "monitor {} lists story {id}, which it neither alerted on nor marked",
                monitor.id
            );
        }
        // And the window is honoured: a wider window can only add rows, never remove one.
        let wider = archive.monitor_stories(monitor.id, 24 * 30, LIMIT);
        let wider_ids: Vec<i64> = wider.iter().map(|story| story.id).collect();
        assert!(
            ids.iter().all(|id| wider_ids.contains(id)),
            "monitor {}: the {HOURS}h page is not a subset of the 30-day page",
            monitor.id
        );
    }
}

/// A label nothing is filed under and a monitor that does not exist answer an empty list
/// on the real archive too — a click on a stale row is a sentence, not a panic or an error.
#[test]
fn a_label_nothing_is_filed_under_and_an_unknown_monitor_are_empty() {
    let Some(archive) = open() else {
        return;
    };

    assert!(archive.place_stories("Nowhere at all, 4711", HOURS, LIMIT).is_empty());
    assert!(archive.place_stories("", HOURS, LIMIT).is_empty());
    assert!(archive.place_stories("United States", HOURS, 0).is_empty());
    assert!(archive.monitor_stories(9_999_999, HOURS, LIMIT).is_empty());
    assert!(archive.monitor_stories(0, HOURS, LIMIT).is_empty());
    assert!(archive.monitor_stories(1, HOURS, 0).is_empty());
}
