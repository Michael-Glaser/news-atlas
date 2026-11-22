//! What the archive layer promises, pinned: the window bound, the search, the pins,
//! the edges, the counts, and that a read-only archive really is read-only.
//!
//! Every test here owns its own in-memory archive with only the columns the
//! statements read, except two that need a file: the read-only guard and the
//! measurement against the checkout's own archive (release only).

use std::collections::HashMap;

use super::*;

/// The inverse of [`iso_from_epoch`], for the tests only: a stamp back to
/// seconds, so the window test can assert the *duration* it got and not the text
/// it was written as.
fn epoch_from_iso(stamp: &str) -> i64 {
    let bytes = stamp.as_bytes();
    assert_eq!(bytes.len(), 20, "not the archive's format: {stamp}");
    let field = |from: usize, to: usize| -> i64 {
        stamp[from..to].parse().expect("a number in the stamp")
    };
    let (year, month, day) = (field(0, 4), field(5, 7) as u32, field(8, 10) as u32);
    let secs = field(11, 13) * 3_600 + field(14, 16) * 60 + field(17, 19);
    days_from_civil(year, month, day) * 86_400 + secs
}

/// Civil date to days since the epoch: Hinnant's `days_from_civil`, the partner
/// of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A stamp `hours` before now, in the archive's format.
fn hours_ago(hours: i64) -> String {
    iso_from_epoch(now_secs() - hours * 3_600)
}

/// An hour inside the 72 h window, and one outside it.
fn inside() -> String {
    hours_ago(1)
}

fn outside() -> String {
    hours_ago(100)
}

/// The archive's tables, with only the columns the statements read.
fn fixture_with_monitors(monitors: bool) -> Archive {
    let conn = Connection::open_in_memory().expect("an in-memory archive");
    conn.execute_batch(
        "CREATE TABLE articles(id INTEGER PRIMARY KEY, source_id INTEGER, url TEXT,
             title TEXT, summary TEXT, content TEXT, publisher TEXT, published TEXT,
             fetched TEXT, iso TEXT, lat REAL, lon REAL, place TEXT,
             relevant INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0, full_text INTEGER DEFAULT 0);
         CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE entities(id INTEGER PRIMARY KEY, name TEXT, type TEXT,
             count INTEGER DEFAULT 1, first_seen TEXT, last_seen TEXT);
         CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL DEFAULT 1,
             type TEXT DEFAULT 'cooccur', label TEXT, PRIMARY KEY(a, b, type));
         CREATE TABLE article_links(a INTEGER, b INTEGER, shared INTEGER, PRIMARY KEY(a, b));
         CREATE TABLE topics(id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, exclude TEXT,
             min_matches INTEGER DEFAULT 1, active INTEGER DEFAULT 1, created TEXT,
             hits INTEGER DEFAULT 0, last_hit TEXT);",
    )
    .expect("schema");
    if monitors {
        conn.execute_batch(
            "CREATE TABLE monitors(id INTEGER PRIMARY KEY, name TEXT, query TEXT,
                 active INTEGER DEFAULT 1, created TEXT, last_run TEXT,
                 hits INTEGER DEFAULT 0, topic_id INTEGER, cadence_minutes INTEGER DEFAULT 0);
             CREATE TABLE alerts(id INTEGER PRIMARY KEY, monitor_id INTEGER, article_id INTEGER,
                 url TEXT, created TEXT);
             CREATE TABLE monitor_seen(monitor_id INTEGER, key TEXT,
                 PRIMARY KEY(monitor_id, key));",
        )
        .expect("monitor schema");
    }
    Archive {
        conn,
        path: PathBuf::from(":memory:"),
        read_error: RefCell::new(None),
    }
}

fn fixture() -> Archive {
    fixture_with_monitors(true)
}

/// One `monitor_seen` mark, with the key the tool protocol writes
/// (`atlas-monitor/src/candidates.rs`, `Candidate::key`: `a<article id>`).
///
/// Spelled here rather than hidden behind a helper on [`Archive`] because the *key* is
/// what a mark is: the table holds marks for things that never became articles too
/// (`w<hash>` rows in the shipped archive), and a test that only ever inserted article
/// marks could not see the difference.
fn mark(archive: &Archive, monitor_id: i64, key: &str) {
    archive
        .conn
        .execute(
            "INSERT OR IGNORE INTO monitor_seen(monitor_id, key) VALUES(?, ?)",
            rusqlite::params![monitor_id, key],
        )
        .expect("a monitor mark");
}

/// One alert row, pointing at a stored story — what a monitor *surfaced*.
fn alert(archive: &Archive, monitor_id: i64, article_id: i64) {
    archive
        .conn
        .execute(
            "INSERT INTO alerts(monitor_id, article_id, url) VALUES(?, ?, '')",
            rusqlite::params![monitor_id, article_id],
        )
        .expect("an alert");
}

/// One stored story; `place`/`lat`/`lon` are what the geocoder recorded.
#[allow(clippy::too_many_arguments)]
fn story(
    archive: &Archive,
    title: &str,
    summary: &str,
    content: &str,
    published: &str,
    place: &str,
    iso: &str,
    lat: f64,
    lon: f64,
) {
    story_id(archive, title, summary, content, published, place, iso, lat, lon);
}

/// [`story`], answering the id the archive gave it: a pin's destination is an id, so a
/// test that pins one has to know which story it pointed at.
#[allow(clippy::too_many_arguments)]
fn story_id(
    archive: &Archive,
    title: &str,
    summary: &str,
    content: &str,
    published: &str,
    place: &str,
    iso: &str,
    lat: f64,
    lon: f64,
) -> i64 {
    archive
        .conn
        .execute(
            "INSERT INTO articles(title, summary, content, published, place, iso, lat, lon, \
             relevant, dupe) VALUES(?,?,?,?,?,?,?,?,1,0)",
            rusqlite::params![title, summary, content, published, place, iso, lat, lon],
        )
        .expect("a story");
    archive.conn.last_insert_rowid()
}

#[test]
fn the_window_bound_is_exactly_the_hours_asked_for_across_every_boundary() {
    // The point of the test is the *duration*, so every case parses the stamp back
    // and compares seconds. UTC has no DST, and this is arithmetic on seconds, so
    // a day, month, year or leap-day boundary cannot move it.
    for (now, hours, expected) in [
        // A day boundary, with the time of day kept: 00:30 three days earlier,
        // not midnight and not 73.5 hours.
        ("2026-11-21T00:30:00Z", 72, "2026-11-18T00:30:00Z"),
        ("2026-11-21T00:00:00Z", 72, "2026-11-18T00:00:00Z"),
        // A month and a year boundary.
        ("2026-01-01T00:00:00Z", 72, "2025-12-29T00:00:00Z"),
        // A leap day: 2028 is a leap year, and 03-01 minus a day is 02-29.
        ("2028-03-01T00:00:00Z", 24, "2028-02-29T00:00:00Z"),
        // The other windows the rail offers, across a day boundary each.
        ("2026-11-21T10:00:00Z", 24, "2026-11-20T10:00:00Z"),
        ("2026-11-21T10:00:00Z", 168, "2026-11-14T10:00:00Z"),
    ] {
        let from = epoch_from_iso(now);
        let bound = window_start_iso_at(from, hours);
        assert_eq!(bound, expected, "{now} minus {hours} h");
        assert_eq!(
            from - epoch_from_iso(&bound),
            hours * 3_600,
            "{bound} is not exactly {hours} hours before {now}"
        );
    }
    // A negative window never reaches into the future.
    assert_eq!(
        window_start_iso_at(epoch_from_iso("2026-11-21T10:00:00Z"), -5),
        "2026-11-21T10:00:00Z"
    );
}

#[test]
fn a_known_day_maps_to_its_civil_date_and_back() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    for days in [-1_000_i64, 0, 1, 59, 60, 19_000, 20_454, 21_000] {
        let (year, month, day) = civil_from_days(days);
        assert_eq!(days_from_civil(year, month, day), days, "{year}-{month}-{day}");
    }
    assert_eq!(iso_from_epoch(0), "1970-01-01T00:00:00Z");
}

#[test]
fn a_search_for_a_wildcard_is_a_search_for_that_character() {
    // `%` and `_` are what `LIKE` reads as patterns; escaped, they are the text the
    // reader typed. Bound raw, this query returned the whole window for a lone `%`.
    assert_eq!(like_pattern("harvest"), "%harvest%");
    assert_eq!(like_pattern("100%"), "%100\\%%");
    assert_eq!(like_pattern("a_b"), "%a\\_b%");
    assert_eq!(like_pattern("c:\\x"), "%c:\\\\x%");

    let archive = fixture();
    story(&archive, "Chip exports rise", "Exports rose.", "Body about shipments.",
        &inside(), "Taipei", "TWN", 25.03, 121.57);
    story(&archive, "A 50% duty on steel", "A tariff.", "The duty is 50% now.",
        &inside(), "Berlin", "DEU", 52.52, 13.40);
    story(&archive, "Wheat harvest report", "Fields.", "Farmers said a_b was the code.",
        &inside(), "Paris", "FRA", 48.86, 2.35);
    story(&archive, "Story from last week", "Old.", "Nothing to see.",
        &outside(), "Tokyo", "JPN", 35.68, 139.69);

    let titles = |needle: &str| -> Vec<String> {
        archive
            .articles(72, 400, needle)
            .into_iter()
            .map(|article| article.title)
            .collect()
    };
    let only = |needle: &str, expected: &str| {
        assert_eq!(
            titles(needle),
            vec![expected.to_string()],
            "searching {needle:?}"
        );
    };
    assert_eq!(titles("").len(), 3, "the window, and nothing outside it");
    only("%", "A 50% duty on steel");
    only("_", "Wheat harvest report");
    only("chip", "Chip exports rise");
    only("shipments", "Chip exports rise");
    // `tariff` is only in the summary — the list shows summaries, so the search
    // reads them (it did not).
    only("tariff", "A 50% duty on steel");
    assert!(titles("no-such-word-anywhere").is_empty());
}

#[test]
fn the_search_statement_has_no_like_term_until_the_reader_types_one() {
    // A structural pin without a clock: the empty box must not carry a `LIKE`, and
    // the filled one must carry exactly the three columns the list can show.
    let plain = article_sql(false);
    assert!(!plain.contains("LIKE"), "{plain}");
    assert!(!plain.contains("?2 = "), "no sentinel pattern: {plain}");
    assert_eq!(plain.matches("LIMIT ?2").count(), 1, "{plain}");
    let searching = article_sql(true);
    assert_eq!(searching.matches("LIKE ?2 ESCAPE '\\'").count(), 3, "{searching}");
    assert_eq!(searching.matches("LIMIT ?3").count(), 1, "{searching}");
}

#[test]
fn a_pin_is_the_place_the_archive_named_and_the_origin_is_not_a_place() {
    let archive = fixture();
    // The largest group is the archive saying "no place": 0,0.
    for _ in 0..6 {
        story(&archive, "No place", "s", "c", &inside(), "", "", 0.0, 0.0);
    }
    // One place whose stories were geocoded a street apart: one pin, not two.
    story(&archive, "Lisbon A", "s", "c", &inside(), "Lisbon", "PRT", 38.72, -9.14);
    story(&archive, "Lisbon B", "s", "c", &inside(), "Lisbon", "PRT", 38.70, -9.14);
    story(&archive, "Porto", "s", "c", &inside(), "Porto", "PRT", 41.15, -8.61);
    // Two places that share a geocode: two pins with their own names.
    story(&archive, "United States", "s", "c", &inside(), "United States", "USA", 38.9, -77.04);
    story(&archive, "Washington", "s", "c", &inside(), "Washington", "USA", 38.9, -77.04);
    // Outside every bound: each of these must not become a pin.
    story(&archive, "North pole", "s", "c", &inside(), "Arctic", "NOR", 91.5, 0.0);
    story(&archive, "Old story", "s", "c", &outside(), "Rome", "ITA", 41.9, 12.5);

    // Limit 2: the 0,0 group would have taken the first slot, so a limit applied
    // before the filter hands back one pin instead of two. The order is
    // count then name, so a tie cannot reorder the answer between two runs.
    let pins = archive.pins(72, 2);
    let labels: Vec<&str> = pins.iter().map(|pin| pin.place.as_str()).collect();
    assert_eq!(labels, vec!["Lisbon", "Porto"], "{labels:?}");
    assert_eq!(pins[0].count, 2, "one place, two stories");
    assert!(
        (pins[0].lat - 38.71).abs() < 0.000_1,
        "the centroid of the stories that named it: {}",
        pins[0].lat
    );

    let all = archive.pins(72, 400);
    assert!(
        all.iter().all(|pin| !(pin.lat == 0.0 && pin.lon == 0.0)),
        "0,0 is not a place"
    );
    assert!(
        all.iter().all(|pin| pin.lat.abs() <= 90.0 && pin.lon.abs() <= 180.0),
        "and neither is 91.5° north"
    );
    // Two places that share a geocode stay two pins with their own names; grouping
    // by the rounded coordinate collapsed them into one pin called USA.
    let named: Vec<&str> = all.iter().map(|pin| pin.place.as_str()).collect();
    assert_eq!(named, vec!["Lisbon", "Porto", "United States", "Washington"], "{named:?}");
}

#[test]
fn an_edge_is_drawn_only_between_two_nodes_that_are_drawn() {
    let archive = fixture();
    archive
        .conn
        .execute_batch(
            "INSERT INTO edges(a,b,weight,label) VALUES(1,2,3.0,'x'), (2,3,2.0,'y'), \
             (1,4,9.0,'out of the set'), (3,1,1.0,'z');",
        )
        .expect("edges");
    let pairs = |edges: Vec<Edge>| -> Vec<(i64, i64)> {
        edges.into_iter().map(|edge| (edge.a, edge.b)).collect()
    };
    // Both ends in {1,2,3}: (1,2), (2,3), (3,1) — and not (1,4).
    let mut found = pairs(archive.edges(&[1, 2, 3], 900));
    found.sort_unstable();
    assert_eq!(found, vec![(1, 2), (2, 3), (3, 1)]);
    assert_eq!(pairs(archive.edges(&[1, 2], 900)), vec![(1, 2)]);
    // The limit applies after the set, heaviest first.
    assert_eq!(pairs(archive.edges(&[1, 2, 3], 1)), vec![(1, 2)]);
    assert!(archive.edges(&[], 900).is_empty());
}

#[test]
fn one_query_reports_the_counts_and_an_archive_without_monitors_still_answers() {
    let archive = fixture();
    story(&archive, "One", "s", "c", &inside(), "Rome", "ITA", 41.9, 12.5);
    story(&archive, "Two", "s", "c", &inside(), "Rome", "ITA", 41.9, 12.5);
    archive
        .conn
        .execute_batch(
            "INSERT INTO entities(name,type,last_seen) VALUES('A','ORG', '2026-11-21T00:00:00Z');
             INSERT INTO edges(a,b,weight) VALUES(1,2,1.0);
             INSERT INTO article_links(a,b,shared) VALUES(1,2,1);
             INSERT INTO topics(name,keywords) VALUES('Chips','chip');
             INSERT INTO monitors(name,query) VALUES('Watch','chips');
             INSERT INTO alerts(monitor_id,url,created) VALUES(1,'https://x.test/a','2026-11-21');",
        )
        .expect("the rest");
    let counts = archive.counts();
    assert_eq!((counts.articles, counts.relevant), (2, 2));
    assert_eq!((counts.entities, counts.edges, counts.links), (1, 1, 1));
    assert_eq!((counts.topics, counts.monitors, counts.alerts), (1, 1, 1));

    // An archive written before the monitor half existed: the article count is
    // still the article count, and the missing tables read as zero.
    let older = fixture_with_monitors(false);
    story(&older, "One", "s", "c", &inside(), "Rome", "ITA", 41.9, 12.5);
    let counts = older.counts();
    assert_eq!((counts.articles, counts.relevant), (1, 1), "not zeroed by a missing table");
    assert_eq!((counts.monitors, counts.alerts), (0, 0));
}

#[test]
fn an_archive_opened_read_only_refuses_a_write() {
    // The guard is `PRAGMA query_only`, and it used to be *read* and thrown away, so
    // nothing proved it was on. A write has to fail here.
    let dir = std::env::temp_dir().join(format!("atlas-desktop-data-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("atlas.db");
    {
        let writable = Connection::open(&path).expect("create");
        writable
            .execute_batch(
                "CREATE TABLE articles(id INTEGER PRIMARY KEY, title TEXT, relevant INTEGER,
                     dupe INTEGER, published TEXT);",
            )
            .expect("schema");
    }
    let archive = Archive::open(&path).expect("open read-only");
    let refused = archive
        .conn
        .execute("DELETE FROM articles", [])
        .expect_err("a read-only archive must refuse a write");
    let message = refused.to_string().to_lowercase();
    assert!(
        message.contains("readonly") || message.contains("read-only") || message.contains("query_only"),
        "{message}"
    );
    // And the same connection still reads.
    assert_eq!(archive.counts().articles, 0);
    drop(archive);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reading_the_real_archive_is_quick_enough_to_type_with() {
    // The number behind the module doc's table. It reads the checkout's own
    // archive, read-only, and says so instead of failing when there is none (a
    // fresh clone, a CI box): the assertions are structural.
    //
    // Release only: a debug build's Strings and allocator are not the build that
    // ships, so its milliseconds would answer a question nobody asked — and
    // reading 400 full bodies 30 times made every `cargo test` on a shared
    // checkout pay for a measurement only the release run needs.
    if cfg!(debug_assertions) {
        println!(
            "skipped: a debug build's timings are not the shipped ones — \
             `cargo test --release -p atlas-desktop reading_the_real_archive`"
        );
        return;
    }
    let path = crate::assets::archive();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return;
    }
    let archive = match Archive::open(&path) {
        Ok(archive) => archive,
        Err(problem) => {
            println!("skipped: {problem}");
            return;
        }
    };
    let counts = archive.counts();
    if counts.articles == 0 {
        println!("skipped: {} holds no articles", path.display());
        return;
    }
    let bound = window_start_iso(72);
    let median = |mut times: Vec<f64>| {
        times.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
        (times[times.len() / 2], times[times.len() * 95 / 100])
    };
    // Both needles are taken from the archive instead of being spelled out here: a
    // word written into a test is a word that goes stale the first time the archive
    // no longer contains it, and what this times is a search that hits. The miss is
    // that same word with two letters appended — the shape of a hit, no rows.
    let Some(newest) = archive.articles(72, 1, "").into_iter().next() else {
        println!("skipped: no story inside the window to take a search word from");
        return;
    };
    let word = newest
        .title
        .split(|character: char| !character.is_alphanumeric())
        .max_by_key(|word| word.len())
        .filter(|word| word.len() >= 4)
        .unwrap_or(newest.title.as_str())
        .to_string();
    if word.is_empty() {
        println!("skipped: the newest story has no title to take a search word from");
        return;
    }
    let miss = format!("{word}qq");
    let mut lines: Vec<String> = Vec::new();
    for (label, needle) in [
        ("empty box", ""),
        ("one word", word.as_str()),
        ("no hits", miss.as_str()),
    ] {
        for _ in 0..3 {
            archive.articles(72, 400, needle);
        }
        let mut times = Vec::with_capacity(10);
        let mut rows = 0;
        for _ in 0..10 {
            let started = std::time::Instant::now();
            let found = archive.articles(72, 400, needle);
            times.push(started.elapsed().as_secs_f64() * 1_000.0);
            rows = found.len();
            for article in &found {
                assert!(
                    article.published.as_str() >= bound.as_str(),
                    "{} is outside the window {bound}",
                    article.published
                );
            }
        }
        let (p50, p95) = median(times);
        lines.push(format!("{label}: {rows} rows, p50 {p50:.2} ms, p95 {p95:.2} ms"));
    }
    println!(
        "atlas-desktop: articles(72 h, 400) on {} ({} articles) — {}",
        path.display(),
        counts.articles,
        lines.join(" · ")
    );
    assert_eq!(archive.counts().articles, counts.articles, "a read does not write");
}

// ---------------------------------------------------------------------------
// A pin's destination
// ---------------------------------------------------------------------------
// A dot on the map is a *place*, and a reader who clicks it wants the news there: the
// newest story filed at that place, inside the window the map is showing. These tests
// pin the three halves of that sentence — inside the window, at that place, the newest
// one there — from the archive's own rows, and then check the whole answer against the
// definition it claims to compute.

/// One group of the definition: the label, the country it was grouped with, how many
/// stories it holds, the position of them, and the newest one.
#[derive(Debug)]
struct Group {
    label: String,
    iso: String,
    count: i64,
    lat_sum: f64,
    lon_sum: f64,
    newest: i64,
    newest_published: String,
}

impl Group {
    fn lat(&self) -> f64 {
        self.lat_sum / self.count as f64
    }

    fn lon(&self) -> f64 {
        self.lon_sum / self.count as f64
    }
}

/// The window's stories as rows — the definition, spelled out — so a test can group them
/// itself instead of asking the statement to confirm its own answer.
fn window_rows(archive: &Archive, since: &str) -> Vec<(i64, String, String, String, f64, f64)> {
    let mut statement = archive
        .conn
        .prepare(
            "SELECT id, coalesce(published, ''), \
             coalesce(nullif(place, ''), nullif(iso, ''), ''), coalesce(iso, ''), lat, lon \
             FROM articles WHERE relevant = 1 AND dupe = 0 AND published >= ?1 \
             AND lat IS NOT NULL AND lon IS NOT NULL AND NOT (lat = 0 AND lon = 0) \
             AND lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180",
        )
        .expect("the definition, read as rows");
    statement
        .query_map([since], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("rows")
        .flatten()
        .collect()
}

/// [`window_rows`] grouped as the definition groups them: by the label *and* the country
/// it was labelled with, with the newest story by `published` then `id`.
fn defined_groups(rows: &[(i64, String, String, String, f64, f64)]) -> Vec<Group> {
    let mut groups: HashMap<(String, String), Group> = HashMap::new();
    for (id, published, label, iso, lat, lon) in rows {
        let key = (label.clone(), iso.clone());
        let group = groups.entry(key.clone()).or_insert_with(|| Group {
            label: label.clone(),
            iso: iso.clone(),
            count: 0,
            lat_sum: 0.0,
            lon_sum: 0.0,
            newest: *id,
            newest_published: published.clone(),
        });
        group.count += 1;
        group.lat_sum += lat;
        group.lon_sum += lon;
        if (published.as_str(), *id) > (group.newest_published.as_str(), group.newest) {
            group.newest = *id;
            group.newest_published = published.clone();
        }
    }
    groups.into_values().collect()
}

/// Every pin's destination, checked against the stories themselves: the row exists, it is
/// inside the window, it is filed at the pin's place and country, and nothing there is
/// newer. `since` is the bound the pins were asked for; a story an hour old (the fixtures)
/// cannot straddle it, so the second the two reads happened in does not matter.
fn assert_destinations(archive: &Archive, since: &str, pins: &[Pin]) {
    // The membership rule, in one place: the same filters the definition has, so a newer
    // story that could never be in the group cannot be counted as a missed destination.
    const IN_GROUP: &str = "relevant = 1 AND dupe = 0 AND published >= ?3 \
         AND lat IS NOT NULL AND lon IS NOT NULL AND NOT (lat = 0 AND lon = 0) \
         AND lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180";
    for pin in pins {
        let (published, label, iso): (String, String, String) = archive
            .conn
            .query_row(
                "SELECT coalesce(published, ''), \
                 coalesce(nullif(place, ''), nullif(iso, ''), ''), coalesce(iso, '') \
                 FROM articles WHERE id = ?1",
                [pin.article],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "the pin at {} opens story {}, which is not in the archive: {error}",
                    pin.place, pin.article
                )
            });
        assert!(
            published.as_str() >= since,
            "the pin at {} opens {}, filed {} — outside the window {since}",
            pin.place,
            pin.article,
            published
        );
        assert_eq!(
            label, pin.place,
            "the pin at {} opens {}, which is filed at {label}",
            pin.place, pin.article
        );
        let newer: i64 = archive
            .conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM articles WHERE {IN_GROUP} \
                     AND coalesce(nullif(place, ''), nullif(iso, ''), '') = ?1 \
                     AND coalesce(iso, '') = ?2 \
                     AND (published > ?4 OR (published = ?4 AND id > ?5))"
                ),
                rusqlite::params![pin.place, iso, since, published, pin.article],
                |row| row.get(0),
            )
            .expect("the stories filed there");
        assert_eq!(
            newer, 0,
            "the pin at {} opens {}, but {newer} story/stories filed there are newer",
            pin.place, pin.article
        );
    }
}

#[test]
fn a_pin_opens_the_newest_story_of_its_place_inside_the_window() {
    let archive = fixture();
    let _oldest = story_id(&archive, "Lisbon first", "s", "c", &hours_ago(30), "Lisbon", "PRT", 38.72, -9.14);
    let _middle = story_id(&archive, "Lisbon then", "s", "c", &hours_ago(10), "Lisbon", "PRT", 38.71, -9.14);
    let newest = story_id(&archive, "Lisbon now", "s", "c", &hours_ago(2), "Lisbon", "PRT", 38.70, -9.14);
    // Older than the window: still in the archive, still the same place, never a pin's
    // destination because the pin is about the news the window holds.
    let _too_old = story_id(&archive, "Lisbon last week", "s", "c", &outside(), "Lisbon", "PRT", 38.69, -9.14);
    let porto = story_id(&archive, "Porto", "s", "c", &hours_ago(5), "Porto", "PRT", 41.15, -8.61);

    let pins = archive.pins(72, 400);
    let labels: Vec<&str> = pins.iter().map(|pin| pin.place.as_str()).collect();
    assert_eq!(labels, vec!["Lisbon", "Porto"], "{labels:?}");
    assert_eq!(pins[0].article, newest, "the newest story inside the window");
    assert_eq!(pins[0].count, 3, "the story outside the window is not one of them");
    assert_eq!(pins[1].article, porto);
}

#[test]
fn a_pin_breaks_a_tie_by_the_story_filed_last() {
    let archive = fixture();
    // One second, two stories: the map has to answer with one of them, and the answer
    // cannot depend on the order the database happens to return them in.
    let first = story_id(&archive, "Rome A", "s", "c", &inside(), "Rome", "ITA", 41.9, 12.5);
    let second = story_id(&archive, "Rome B", "s", "c", &inside(), "Rome", "ITA", 41.91, 12.5);
    assert!(second > first, "the second story got the higher id");
    let pins = archive.pins(72, 400);
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].article, second, "published desc, then id desc");
}

#[test]
fn two_groups_that_share_a_label_keep_their_own_destination() {
    let archive = fixture();
    // The label is the place the archive named, but the group is the label *and* the
    // country it was named with: two Berlins, one with a country and one without, are two
    // groups — and each opens its own newest story.
    let with_iso = story_id(&archive, "Berlin, Germany", "s", "c", &hours_ago(3), "Berlin", "DEU", 52.52, 13.40);
    let without_iso = story_id(&archive, "Berlin, somewhere", "s", "c", &hours_ago(4), "Berlin", "", 52.50, 13.30);
    let pins = archive.pins(72, 400);
    assert_eq!(pins.len(), 2, "one label, two countries, two pins");
    assert!(pins.iter().all(|pin| pin.place == "Berlin"));
    let mut destinations: Vec<i64> = pins.iter().map(|pin| pin.article).collect();
    destinations.sort_unstable();
    let mut expected = vec![with_iso, without_iso];
    expected.sort_unstable();
    assert_eq!(destinations, expected);
}

#[test]
fn every_pin_on_the_map_points_at_a_story_a_click_can_open() {
    let archive = fixture();
    for _ in 0..6 {
        story(&archive, "No place", "s", "c", &inside(), "", "", 0.0, 0.0);
    }
    story(&archive, "Lisbon", "s", "c", &hours_ago(2), "Lisbon", "PRT", 38.72, -9.14);
    story_id(&archive, "Lisbon older", "s", "c", &hours_ago(20), "Lisbon", "PRT", 38.70, -9.14);
    story_id(&archive, "Hamburg", "s", "c", &hours_ago(7), "Hamburg", "DEU", 53.55, 9.99);
    story(&archive, "Hamburg outside", "s", "c", &outside(), "Hamburg", "DEU", 53.55, 9.99);
    // A place reached only through its country: the label is the code the archive kept.
    story_id(&archive, "Nowhere named", "s", "c", &hours_ago(9), "", "NOR", 59.91, 10.75);

    let since = window_start_iso(72);
    let pins = archive.pins(72, 400);
    let labels: Vec<&str> = pins.iter().map(|pin| pin.place.as_str()).collect();
    assert_eq!(labels, vec!["Lisbon", "Hamburg", "NOR"], "{labels:?}");
    assert_destinations(&archive, &since, &pins);
}

#[test]
fn the_pins_are_the_groups_the_definition_names() {
    // The shipped archive, read-only: the one data set where "is this the definition's
    // answer" has enough rows to mean anything. It says so and passes on a checkout
    // without an archive, like the other measurements here.
    let path = crate::assets::archive();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return;
    }
    let archive = match Archive::open(&path) {
        Ok(archive) => archive,
        Err(problem) => {
            println!("skipped: {problem}");
            return;
        }
    };
    if archive.counts().articles == 0 {
        println!("skipped: {} holds no articles", path.display());
        return;
    }

    // The statement takes its window bound from the clock, so the comparison takes its
    // own from the same second: a bound that moved between the two reads would describe a
    // different window, and the difference reported would be the calendar, not the query.
    let mut attempts = 0;
    let (since, pins) = loop {
        attempts += 1;
        let since = window_start_iso(72);
        let pins = archive.pins(72, 400);
        if window_start_iso(72) == since {
            break (since, pins);
        }
        assert!(attempts < 5, "the window bound moved five seconds in a row");
    };

    let rows = window_rows(&archive, &since);
    assert!(
        !rows.is_empty(),
        "the 72 h window holds no story, so there is nothing to group"
    );
    let groups = defined_groups(&rows);
    assert!(
        groups.len() <= 400,
        "{} places in the window, more than the map asks for: the limit would cut the comparison",
        groups.len()
    );

    // The answer is the definition's groups: one pin per label-and-country, with its size
    // and the id of its newest story.
    let mut found: Vec<(String, i64, i64)> =
        pins.iter().map(|pin| (pin.place.clone(), pin.count, pin.article)).collect();
    let mut defined: Vec<(String, i64, i64)> = groups
        .iter()
        .map(|group| (group.label.clone(), group.count, group.newest))
        .collect();
    found.sort();
    defined.sort();
    assert_eq!(found, defined, "the pins are not the groups the definition names");

    // The position is the mean of the stories that named the place, at the four decimals
    // the map draws with.
    for group in &groups {
        let pin = pins
            .iter()
            .find(|pin| pin.place == group.label && pin.article == group.newest)
            .unwrap_or_else(|| panic!("no pin for {} opening {}", group.label, group.newest));
        assert!(
            (pin.lat - group.lat()).abs() <= 1.0e-4 && (pin.lon - group.lon()).abs() <= 1.0e-4,
            "the pin for {} is at {},{}, the mean of its stories is {},{}",
            group.label,
            pin.lat,
            pin.lon,
            group.lat(),
            group.lon()
        );
    }

    // Heaviest first, and among equals by label: the order the filter row under the map
    // reads down. Two groups that share a label are a tie this cannot order, so the
    // comparison is on the pair.
    let keys: Vec<(i64, &str)> = pins.iter().map(|pin| (-pin.count, pin.place.as_str())).collect();
    assert!(
        keys.windows(2).all(|pair| pair[0] <= pair[1]),
        "the pins are not ordered by count then name: {keys:?}"
    );

    assert_destinations(&archive, &since, &pins);
}

// ---------------------------------------------------------------------------
// a row in a list opens its stories: a place, and a monitor
// ---------------------------------------------------------------------------

/// The label a place is *named* by is the pins' grouping, so a dot and the page it opens
/// enumerate the same rows — and both are ordered like the Articles page.
#[test]
fn a_place_opens_its_own_stories_newest_first_and_only_from_the_window() {
    let archive = fixture();
    // Two stories share a publication time, so the id tiebreak is observable.
    let newest = story_id(&archive, "newest", "", "", &inside(), "Washington", "USA", 38.9, -77.0);
    // Three hours back, not two: a story stamped exactly at a window's edge is inside or
    // outside depending on which second the window was computed in, and a test that hangs
    // on that is a test that fails on a slow day.
    let tie_a = story_id(&archive, "tie a", "", "", &hours_ago(3), "Washington", "USA", 38.9, -77.0);
    let tie_b = story_id(&archive, "tie b", "", "", &hours_ago(3), "Washington", "USA", 38.9, -77.0);
    let old = story_id(&archive, "old", "", "", &outside(), "Washington", "USA", 38.9, -77.0);
    // A story the geocoder only placed in the country, and one in another place.
    let country_only = story_id(&archive, "country only", "", "", &inside(), "", "USA", 0.0, 0.0);
    let china = story_id(&archive, "china", "", "", &inside(), "China", "CHN", 39.9, 116.4);

    let found: Vec<i64> = archive
        .place_stories("Washington", 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(
        found,
        vec![newest, tie_b, tie_a],
        "a place's stories are not newest first with the id tiebreak, or the window story \
         ({old}) or another place leaked in"
    );

    // The window is the list's: at two hours, only the story filed inside the hour.
    let narrow: Vec<i64> = archive
        .place_stories("Washington", 2, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(narrow, vec![newest]);

    // The limit is honoured, on the ordered list.
    let limited: Vec<i64> = archive
        .place_stories("Washington", 72, 2)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(limited, vec![newest, tie_b]);

    // `place` wins over `iso`: a story that names a place does not also belong to the
    // country it is in, and a story that names no place belongs to its country's label.
    let usa: Vec<i64> = archive
        .place_stories("USA", 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(usa, vec![country_only], "the country label took a named place's stories");
    let china_rows: Vec<i64> = archive
        .place_stories("China", 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(china_rows, vec![china]);

    // The rows carry the columns an [`Article`] row carries, so a page can render them
    // the way the list does: the outlet label falls back to the publisher and then to
    // "unknown source", never to a `?`.
    let opened = archive.place_stories("Washington", 72, 1);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].title, "newest");
    assert_eq!(opened[0].source, "");
    assert_eq!(opened[0].country, "USA");
}

/// A place with no story this window, a label nothing is filed under, and an empty label
/// are each an empty list: a dot that opens nothing is a sentence, not an error.
#[test]
fn a_place_with_no_story_answers_an_empty_list() {
    let archive = fixture();
    story_id(&archive, "old", "", "", &outside(), "Washington", "USA", 38.9, -77.0);
    story_id(&archive, "nameless", "", "", &inside(), "", "", 0.0, 0.0);

    // The story exists, but not in this window.
    assert!(archive.place_stories("Washington", 72, 400).is_empty());
    // Nothing is filed under this label at all.
    assert!(archive.place_stories("Atlantis", 72, 400).is_empty());
    // `''` is the archive saying "no place", not a place (pins drops those rows).
    assert!(archive.place_stories("", 72, 400).is_empty());
    assert!(archive.place_stories("   ", 72, 400).is_empty());
    // And nothing is not an error: a page asking for zero rows gets zero rows.
    assert!(archive.place_stories("Washington", 72, 0).is_empty());
}

/// A monitor's stories are what it alerted on *and* what it already judged, and only the
/// marks that name an article are articles.
#[test]
fn a_monitor_opens_the_stories_it_surfaced_and_the_stories_it_judged() {
    let archive = fixture();
    let surfaced = story_id(&archive, "surfaced", "", "", &inside(), "", "", 0.0, 0.0);
    let judged = story_id(&archive, "judged", "", "", &hours_ago(4), "", "", 0.0, 0.0);
    let both = story_id(&archive, "both", "", "", &hours_ago(2), "", "", 0.0, 0.0);
    let stale = story_id(&archive, "stale", "", "", &outside(), "", "", 0.0, 0.0);
    let unmarked = story_id(&archive, "unmarked", "", "", &inside(), "", "", 0.0, 0.0);

    mark(&archive, 1, &format!("a{judged}"));
    mark(&archive, 1, &format!("a{both}"));
    mark(&archive, 1, &format!("a{stale}"));
    // A mark that is not an article id: a page the monitor judged that never became a
    // row in `articles` (16 of the shipped archive's 22 marks are this shape).
    mark(&archive, 1, "w515b0f6b6dacbd41");
    // One story reached by both tables: an alert and a mark must not list it twice.
    mark(&archive, 1, &format!("a{both}"));
    alert(&archive, 1, surfaced);
    alert(&archive, 1, both);
    // Another monitor's marks are not this monitor's stories.
    mark(&archive, 2, &format!("a{unmarked}"));

    let found: Vec<i64> = archive
        .monitor_stories(1, 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(
        found,
        vec![surfaced, both, judged],
        "the monitor's stories are not its alerts union its marks, newest first, deduplicated"
    );
    // The stale mark is outside the window, and the `w…` mark is not an article.
    assert!(!found.contains(&stale), "a story outside the window was listed");
    assert!(!found.contains(&unmarked), "another monitor's mark was listed");

    // Limit, window and the two empty answers.
    assert_eq!(archive.monitor_stories(1, 72, 1).len(), 1);
    assert_eq!(
        archive.monitor_stories(1, 72, 1)[0].id,
        surfaced,
        "the limit did not take the newest"
    );
    // Three hours back: `surfaced` and `both` are inside it, `judged` is an hour outside.
    assert_eq!(archive.monitor_stories(1, 3, 400).len(), 2);
    // Monitor 2 marked exactly one story, and that is its page: the marks are per
    // monitor, not a shared pool.
    let theirs: Vec<i64> = archive
        .monitor_stories(2, 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(theirs, vec![unmarked]);
    assert!(archive.monitor_stories(999, 72, 400).is_empty());
    assert!(archive.monitor_stories(0, 72, 400).is_empty());
    assert!(archive.monitor_stories(1, 72, 0).is_empty());
}

/// A monitor's page has to work on an archive that has no monitor half at all: the
/// tables are the collector's, and a window over an older file must not fail.
#[test]
fn a_monitor_read_on_an_archive_without_the_monitor_tables_is_empty() {
    let archive = fixture_with_monitors(false);
    story_id(&archive, "one", "", "", &inside(), "", "", 0.0, 0.0);
    assert!(archive.monitor_stories(1, 72, 400).is_empty());
}

/// The country rollup counts by `iso`, so its row opens the country's stories — including
/// the ones filed under a city, which is what [`Archive::place_stories`] must *not* answer
/// for the same text.
#[test]
fn a_country_opens_its_stories_including_the_ones_filed_under_a_city() {
    let archive = fixture();
    let washington =
        story_id(&archive, "washington", "", "", &inside(), "Washington", "USA", 38.9, -77.0);
    // Three and five hours back: every gap is at least an hour wide, so no assertion
    // here hangs on which second a window was computed in.
    let country_only = story_id(&archive, "country only", "", "", &hours_ago(3), "", "USA", 0.0, 0.0);
    let new_york =
        story_id(&archive, "new york", "", "", &hours_ago(5), "New York", "USA", 40.7, -74.0);
    let china = story_id(&archive, "china", "", "", &inside(), "China", "CHN", 39.9, 116.4);
    let old = story_id(&archive, "old", "", "", &outside(), "Boston", "USA", 42.4, -71.1);

    let found: Vec<i64> = archive
        .country_stories("USA", 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(
        found,
        vec![washington, country_only, new_york],
        "the country's page is not its window stories newest first with the id tiebreak"
    );
    assert!(!found.contains(&china), "another country's story was listed");
    assert!(!found.contains(&old), "a story outside the window was listed");

    // The rollup's own number and the page's length are the same number.
    let rollup = archive.countries(72, 14);
    let usa = rollup
        .iter()
        .find(|(iso, _)| iso == "USA")
        .expect("a USA row in the rollup");
    assert_eq!(usa.1, 3, "the rollup counts three stories in the window");
    assert_eq!(found.len() as i64, usa.1);

    // And the text "USA" is not a *place* on this archive: `place_stories` answers the
    // stories that name no place at all, which is why the rollup needs its own read.
    let as_a_place: Vec<i64> = archive
        .place_stories("USA", 72, 400)
        .iter()
        .map(|article| article.id)
        .collect();
    assert_eq!(as_a_place, vec![country_only]);

    // Limit, window, and the empty answers.
    assert_eq!(archive.country_stories("USA", 72, 2).len(), 2);
    // Two hours back: only the story filed inside the hour.
    assert_eq!(archive.country_stories("USA", 2, 400).len(), 1);
    assert!(archive.country_stories("GBR", 72, 400).is_empty());
    assert!(archive.country_stories("", 72, 400).is_empty());
    assert!(archive.country_stories("  ", 72, 400).is_empty());
    assert!(archive.country_stories("USA", 72, 0).is_empty());
}

// ---------------------------------------------------------------------------------------
// The audit: five rules, one test each, each on an archive of its own.
// ---------------------------------------------------------------------------------------

/// Every table the reads in `data.rs` touch, including the four the shared [`fixture`]
/// predates — `article_countries`, `article_topics`, `briefings` and `meta`.
///
/// One schema string, used by the in-memory archives and by the two tests that need a real
/// file, so a read that grows a table cannot pass here and fail on the shipped archive.
const AUDIT_SCHEMA: &str = "CREATE TABLE articles(id INTEGER PRIMARY KEY, source_id INTEGER,
         url TEXT, title TEXT, summary TEXT, content TEXT, publisher TEXT, published TEXT,
         fetched TEXT, iso TEXT, lat REAL, lon REAL, place TEXT,
         relevant INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0, full_text INTEGER DEFAULT 0);
     CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
     CREATE TABLE article_countries(article_id INTEGER, iso TEXT, is_primary INTEGER DEFAULT 0,
         PRIMARY KEY(article_id, iso));
     CREATE TABLE article_topics(article_id INTEGER, topic_id INTEGER,
         PRIMARY KEY(article_id, topic_id));
     CREATE TABLE entities(id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER DEFAULT 1,
         first_seen TEXT, last_seen TEXT);
     CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL DEFAULT 1, type TEXT DEFAULT 'cooccur',
         label TEXT, PRIMARY KEY(a, b, type));
     CREATE TABLE article_links(a INTEGER, b INTEGER, shared INTEGER, PRIMARY KEY(a, b));
     CREATE TABLE topics(id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, exclude TEXT,
         min_matches INTEGER DEFAULT 1, active INTEGER DEFAULT 1, created TEXT,
         hits INTEGER DEFAULT 0, last_hit TEXT);
     CREATE TABLE monitors(id INTEGER PRIMARY KEY, name TEXT, query TEXT,
         active INTEGER DEFAULT 1, created TEXT, last_run TEXT, hits INTEGER DEFAULT 0,
         topic_id INTEGER, cadence_minutes INTEGER DEFAULT 0);
     CREATE TABLE alerts(id INTEGER PRIMARY KEY, monitor_id INTEGER, article_id INTEGER,
         url TEXT, created TEXT);
     CREATE TABLE monitor_seen(monitor_id INTEGER, key TEXT, PRIMARY KEY(monitor_id, key));
     CREATE TABLE briefings(scope TEXT, key TEXT, hours INTEGER, watermark INTEGER,
         created TEXT, line TEXT, bullets TEXT, articles INTEGER, model TEXT, ms INTEGER,
         PRIMARY KEY(scope, key, hours));
     CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT);";

/// [`AUDIT_SCHEMA`] in memory: the archive every audit read can answer from.
fn audit_archive() -> Archive {
    let conn = Connection::open_in_memory().expect("an in-memory archive");
    conn.execute_batch(AUDIT_SCHEMA).expect("the audit schema");
    Archive {
        conn,
        path: PathBuf::from(":memory:"),
        read_error: RefCell::new(None),
    }
}

/// A directory of this test's own, removed when it drops.
///
/// The pid and a counter keep two tests running at once apart (the clock's resolution is
/// coarser than a thread switch), and the guard removes the file even when a test panics,
/// so a failing audit leaves nothing in `/tmp` for the next run to read.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-data-{tag}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch { dir }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("atlas.db")
    }

    /// Write `schema` into `atlas.db`, run `seed` on the writable connection, then hand the
    /// file back read-only through [`Archive::open`] — the road the window takes.
    fn archive_with(&self, schema: &str, seed: impl FnOnce(&Connection)) -> Archive {
        {
            let writable = Connection::open(self.file()).expect("a scratch database");
            writable.execute_batch(schema).expect("the audit schema");
            seed(&writable);
        }
        Archive::open(self.file()).expect("the archive opens read-only")
    }

    fn archive(&self, seed: impl FnOnce(&Connection)) -> Archive {
        self.archive_with(AUDIT_SCHEMA, seed)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// The ids of a story list, in the order it answered.
fn ids(stories: &[Article]) -> Vec<i64> {
    stories.iter().map(|story| story.id).collect()
}

/// Rule 1: every read answers the rows its name promises.
///
/// One archive, seeded with every relationship the reads can express: a country's own story,
/// a story about a second country, a place with two stories, a story with no place, a story
/// outside the window, a duplicate, a topic's stories, a monitor's alert and mark, two
/// connected entities and one that is not, and one briefing.
#[test]
fn every_read_answers_the_rows_its_name_promises() {
    let archive = audit_archive();
    let seed = hours_ago(1);
    let berlin = story_id(
        &archive, "Berlin summit opens", "chips", "talks", &hours_ago(2), "Berlin", "DEU", 52.52, 13.40,
    );
    let beijing = story_id(
        &archive, "Beijing chip rules", "", "chips", &hours_ago(3), "Beijing", "CHN", 39.90, 116.40,
    );
    let kyiv = story_id(
        &archive, "Kyiv talks resume", "", "", &hours_ago(4), "Kyiv", "UKR", 50.45, 30.52,
    );
    let strike = story_id(
        &archive, "Berlin transit strike", "", "trains", &hours_ago(5), "Berlin", "DEU", 52.50, 13.40,
    );
    story_id(&archive, "A story with no place", "", "", &hours_ago(6), "", "", 0.0, 0.0);
    let old = story_id(
        &archive, "Berlin last week", "", "", &hours_ago(100), "Berlin", "DEU", 52.52, 13.40,
    );
    let dupe = story_id(
        &archive, "Berlin summit opens (copy)", "", "", &hours_ago(1), "Berlin", "DEU", 52.52, 13.40,
    );
    archive
        .conn
        .execute_batch(&format!(
            "INSERT INTO sources(id, name) VALUES(1, 'Alpha Desk');
             UPDATE articles SET dupe = 1 WHERE id = {dupe};
             UPDATE articles SET source_id = 1, url = 'https://example.test/{berlin}', full_text = 1
                 WHERE id = {berlin};
             INSERT INTO article_countries(article_id, iso, is_primary) VALUES
                 ({berlin}, 'DEU', 1), ({berlin}, 'FRA', 0), ({kyiv}, 'DEU', 0);
             INSERT INTO article_topics(article_id, topic_id) VALUES
                 ({berlin}, 1), ({beijing}, 1), ({strike}, 2);
             INSERT INTO topics(id, name, keywords, active, hits) VALUES
                 (1, 'Chips', 'chip', 1, 4), (2, 'Trains', 'train', 0, 1);
             INSERT INTO entities(id, name, type, count, last_seen) VALUES
                 (1, 'NATO', 'ORG', 5, '{seed}'), (2, 'EU', 'ORG', 3, '{seed}'),
                 (3, 'Unconnected', 'ORG', 9, '{seed}'), (4, 'Partner', 'ORG', 1, '{seed}');
             INSERT INTO edges(a, b, weight) VALUES(1, 2, 2.0), (2, 1, 0.5), (1, 4, 9.0);
             INSERT INTO article_links(a, b, shared) VALUES({berlin}, {beijing}, 1);
             INSERT INTO monitors(id, name, query) VALUES(1, 'Watch', 'chips');
             INSERT INTO alerts(monitor_id, article_id, url) VALUES(1, {berlin}, ''), (1, {old}, '');
             INSERT INTO monitor_seen(monitor_id, key) VALUES(1, 'a{beijing}');
             INSERT INTO briefings(scope, key, hours, watermark, created, line, bullets,
                 articles, model, ms) VALUES('window', '', 72, {berlin}, '{seed}', 'A quiet week.',
                 '[\"one\", \"two\"]', 3, 'test-model', 5);
             INSERT INTO meta(k, v) VALUES('search_engine', 'fts5');"
        ))
        .expect("the relationships");

    // articles: the window, newest first, the duplicate and the old story out, source joined.
    let window = archive.articles(72, 10, "");
    assert_eq!(
        ids(&window),
        vec![berlin, beijing, kyiv, strike, window[4].id],
        "the list is not the window's relevant, non-duplicate stories newest first"
    );
    assert_eq!(
        window[4].title, "A story with no place",
        "the place-less story is not the fifth row"
    );
    assert_eq!(window[0].source, "Alpha Desk", "the source is not joined");
    assert!(window[0].full_text, "full_text is not read");
    assert_eq!(ids(&archive.articles(72, 2, "")), vec![berlin, beijing], "the limit is not taken");
    assert!(
        archive.articles(24, 10, "").iter().all(|story| story.id != old),
        "an old story was in the day's list"
    );

    // …and the search is the reader's text, not the window again.
    assert_eq!(ids(&archive.articles(72, 10, "chips")), vec![berlin, beijing]);

    // country_stories: the country's own stories **and** the ones that name it.
    assert_eq!(ids(&archive.country_stories("DEU", 72, 10)), vec![berlin, kyiv, strike]);
    assert_eq!(ids(&archive.country_stories("FRA", 72, 10)), vec![berlin]);
    assert_eq!(ids(&archive.country_stories("CHN", 72, 10)), vec![beijing]);

    // place_stories: the label the map groups by, not the country.
    assert_eq!(ids(&archive.place_stories("Berlin", 72, 10)), vec![berlin, strike]);
    assert_eq!(ids(&archive.place_stories("Beijing", 72, 10)), vec![beijing]);

    // pins: one per place with a coordinate, its count, and its newest story as the door.
    let pins = archive.pins(72, 10);
    let summary: Vec<(&str, i64, i64)> = pins
        .iter()
        .map(|pin| (pin.place.as_str(), pin.count, pin.article))
        .collect();
    assert_eq!(
        summary,
        vec![("Berlin", 2, berlin), ("Beijing", 1, beijing), ("Kyiv", 1, kyiv)],
        "the pins are not the places with coordinates, heaviest first"
    );

    // countries: the same pairs as country_stories, and a count that is the list's length.
    let countries = archive.countries(72, 100);
    assert_eq!(
        countries,
        vec![
            ("DEU".to_string(), 3),
            ("CHN".to_string(), 1),
            ("FRA".to_string(), 1),
            ("UKR".to_string(), 1),
        ],
        "the rollup is not the (story, country) pairs, count then code"
    );
    for (iso, count) in &countries {
        assert_eq!(
            archive.country_stories(iso, 72, 400).len() as i64,
            *count,
            "the {iso} count and its list disagree"
        );
    }

    // topic_articles: the join the classifier wrote, not a re-match of the keywords.
    assert_eq!(ids(&archive.topic_articles(1, 72, 10)), vec![berlin, beijing]);
    assert_eq!(ids(&archive.topic_articles(2, 72, 10)), vec![strike]);

    // monitor_stories: what it alerted on plus what it marked, deduplicated, in the window.
    assert_eq!(ids(&archive.monitor_stories(1, 72, 10)), vec![berlin, beijing]);

    // entities: the strongest that have a relation; edges: both ends in the set.
    let entities = archive.entities(72, 10);
    let names: Vec<(&str, i64)> =
        entities.iter().map(|entity| (entity.name.as_str(), entity.count)).collect();
    assert_eq!(
        names,
        vec![("NATO", 5), ("EU", 3), ("Partner", 1)],
        "the graph did not drop the unconnected node or did not order by mentions"
    );
    // Both ends must be in the set: (1, 4) is a real relation that this graph does not draw.
    let edges = archive.edges(&[1, 2], 10);
    let lines: Vec<(i64, i64, f64)> = edges.iter().map(|edge| (edge.a, edge.b, edge.weight)).collect();
    assert_eq!(lines, vec![(1, 2, 2.0), (2, 1, 0.5)], "an edge pointed outside the drawn nodes");
    assert_eq!(
        archive.edges(&[1, 2, 3, 4], 10).len(),
        3,
        "the whole set does not hold the third relation"
    );

    // briefing: the row for this scope and window, and none for another window.
    let briefing = archive.briefing("window", "", 72).expect("the briefing it was given");
    assert_eq!(briefing.bullets, vec!["one".to_string(), "two".to_string()]);
    assert_eq!((briefing.articles, briefing.model.as_str()), (3, "test-model"));
    assert!(archive.briefing("window", "", 24).is_none(), "another window answered a row");

    // meta: the key, and an honest `None` for a key the archive never had.
    assert_eq!(archive.meta("search_engine").as_deref(), Some("fts5"));
    assert_eq!(archive.meta("never-written"), None);

    // counts: the tables as the rail counts them.
    let counts = archive.counts();
    assert_eq!((counts.articles, counts.relevant), (7, 7));
    assert_eq!((counts.entities, counts.edges, counts.links), (4, 3, 1));
    assert_eq!((counts.topics, counts.monitors, counts.alerts), (2, 1, 2));

    // A read that answered records nothing, whatever it answered.
    assert_eq!(archive.last_error(), None, "a clean archive reported a failure");
}

/// Rule 2: nothing older than the chosen window, at 24, 72 and 168 hours, and a story dated
/// in the future sorts as the newest without disturbing the order below it.
///
/// The rows sit an hour inside and an hour outside each bound, so the assertion is about the
/// window and not about the second the test ran in.
#[test]
fn nothing_older_than_the_window_and_a_future_story_keeps_the_order() {
    let archive = audit_archive();
    let mut stories: Vec<(i64, String)> = Vec::new();
    for hours in [1_i64, 23, 25, 71, 73, 167, 169] {
        let id = story_id(
            &archive,
            &format!("a story {hours} h old"),
            "",
            "",
            &hours_ago(hours),
            &format!("Place {hours}"),
            &format!("C{hours:02}"),
            10.0 + hours as f64 / 100.0,
            20.0,
        );
        stories.push((id, format!("C{hours:02}")));
    }
    // Dated tomorrow: the publisher's clock is not this window's, and the row still sorts
    // where its stamp says it belongs.
    let future = story_id(
        &archive,
        "a story dated tomorrow",
        "",
        "",
        &iso_from_epoch(now_secs() + 48 * 3_600),
        "Future",
        "CFU",
        0.0,
        0.0,
    );
    // The bound itself, as close as a wall clock allows: a story five seconds inside the
    // day's edge is in, one five seconds outside is out. Zero margin is not testable here —
    // the read takes `now` when it runs, which is microseconds after this line — so the
    // assertion is ±5 s around the edge rather than about the edge exactly.
    let edge_inside = story_id(
        &archive,
        "a story five seconds inside the day",
        "",
        "",
        &iso_from_epoch(now_secs() - 24 * 3_600 + 5),
        "Edge in",
        "CEI",
        30.0,
        30.0,
    );
    let edge_outside = story_id(
        &archive,
        "a story five seconds outside the day",
        "",
        "",
        &iso_from_epoch(now_secs() - 24 * 3_600 - 5),
        "Edge out",
        "CEO",
        31.0,
        31.0,
    );

    let check = |hours: i64| {
        let listed = archive.articles(hours, 100, "");
        let listed_ids: Vec<i64> = listed.iter().map(|story| story.id).collect();
        let bound = window_start_iso(hours);
        assert!(
            listed.iter().all(|story| story.published >= bound),
            "the {hours} h list holds a story older than {bound}: {:?}",
            listed
                .iter()
                .map(|story| (story.id, story.published.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(listed_ids.first(), Some(&future), "the newest stamp is not first at {hours} h");
        assert!(
            listed.windows(2).all(|pair| {
                (pair[0].published.as_str(), pair[0].id) >= (pair[1].published.as_str(), pair[1].id)
            }),
            "the {hours} h list is not newest first with the id tiebreak"
        );
        assert!(
            listed_ids.iter().all(|id| *id != stories[6].0),
            "the 169 h story reached the {hours} h list"
        );
        listed_ids
    };

    // 24 h: 1 h, 23 h and the edge five seconds inside are in; 25 h is out.
    assert_eq!(
        check(24),
        vec![future, stories[0].0, stories[1].0, edge_inside],
        "the day's list is not the future story, the two young ones and the edge"
    );
    assert!(
        !check(24).contains(&edge_outside),
        "a story five seconds older than the day was listed"
    );
    // 72 h: everything up to 71 h, the 24 h edges included.
    assert_eq!(
        check(72),
        vec![
            future,
            stories[0].0,
            stories[1].0,
            edge_inside,
            edge_outside,
            stories[2].0,
            stories[3].0
        ]
    );
    // 168 h: up to 167 h, and never the 169 h story.
    let week = check(168);
    assert_eq!(
        week.len(),
        9,
        "the week did not hold six stories, the two edges and the future one: {week:?}"
    );
    assert!(week.contains(&stories[5].0), "the 167 h story is inside the week");
    assert!(week.contains(&edge_outside), "the five-seconds-outside story is inside the week");

    // The same bound decides the rollup, the pins and a place's list: a window is one rule.
    let countries_24 = archive.countries(24, 100);
    assert_eq!(
        countries_24.len(),
        4,
        "the 24 h rollup is not the future story, the two young ones and the edge: {countries_24:?}"
    );
    assert!(
        archive.pins(24, 100).iter().all(|pin| pin.place != "Place 25"),
        "a pin was drawn for a story outside the day"
    );
    assert!(archive.place_stories("Place 169", 168, 10).is_empty());
    assert_eq!(archive.place_stories("Place 167", 168, 10).len(), 1);
}

/// Rule 3: the file is opened read-only and refuses a write, and a table this archive does
/// not have answers an empty list instead of taking the window down.
#[test]
fn a_read_only_archive_refuses_a_write_and_a_missing_table_answers_empty() {
    let scratch = Scratch::new("read-only");
    let archive = scratch.archive_with(
        // The columns `ARTICLE_COLUMNS` reads and nothing else: this archive is deliberately
        // missing every table but the two a story list needs.
        "CREATE TABLE articles(id INTEGER PRIMARY KEY, source_id INTEGER, title TEXT, publisher TEXT,
             summary TEXT, content TEXT, published TEXT, iso TEXT, url TEXT,
             full_text INTEGER, relevant INTEGER, dupe INTEGER);
         CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);",
        |writable| {
            writable
                .execute(
                    "INSERT INTO articles(title, published, relevant, dupe) \
                     VALUES('one story', '2026-11-21T00:00:00Z', 1, 0)",
                    [],
                )
                .expect("a story");
        },
    );

    let refused = archive
        .conn
        .execute("DELETE FROM articles", [])
        .expect_err("a read-only archive must refuse a write");
    let message = refused.to_string().to_lowercase();
    assert!(
        message.contains("readonly") || message.contains("read-only") || message.contains("query_only"),
        "the write was refused for the wrong reason: {message}"
    );

    // A table the file does not have: empty, and a reason rather than a panic.
    assert!(archive.entities(72, 10).is_empty());
    let error = archive.last_error().expect("a missing table was not recorded");
    assert_eq!(error.read, "entities");
    assert!(
        error.message.contains("no such table: entities"),
        "the reason is not SQLite's: {error}"
    );
    assert!(error.to_string().contains("the entities read failed"));

    // …and the reads that do have their tables still answer, through the same connection.
    let listed = archive.articles(24 * 365 * 50, 10, "");
    assert_eq!(
        listed.len(),
        1,
        "the read that has its tables answered {} rows, and its failure is {:?}",
        listed.len(),
        archive.last_error()
    );
}

/// Rule 4: a failed read is distinguishable from an empty one, and a row that will not
/// decode does not throw away the rows that will.
#[test]
fn a_failed_read_is_not_an_empty_read() {
    let archive = audit_archive();
    story_id(&archive, "one", "", "", &hours_ago(1), "Berlin", "DEU", 52.5, 13.4);

    // An empty answer that is an answer: a search that matches nothing.
    archive.clear_error();
    assert!(archive.articles(72, 10, "no-such-word-anywhere").is_empty());
    assert_eq!(archive.last_error(), None, "a search that found nothing was called a failure");

    // An empty answer that is a failure: the table the query needs is gone.
    archive.conn.execute_batch("DROP TABLE edges").expect("drop for the audit");
    archive.clear_error();
    assert!(archive.edges(&[1, 2], 10).is_empty());
    let error = archive.last_error().expect("a failed read left no trace");
    assert_eq!(error.read, "edges");
    assert!(error.message.contains("no such table: edges"), "{error}");

    // The same shape for a `query_row` read: a row that is not there is an answer, and no
    // table at all is a failure. Both answer `None`; only the second is recorded.
    archive.clear_error();
    assert!(archive.briefing("window", "", 72).is_none(), "an empty briefings table answered a row");
    assert_eq!(
        archive.last_error(),
        None,
        "a briefing the model has not written was reported as a failed read"
    );
    archive.conn.execute_batch("DROP TABLE briefings").expect("drop for the audit");
    archive.clear_error();
    assert!(archive.briefing("window", "", 72).is_none());
    assert_eq!(
        archive.last_error().map(|error| error.read),
        Some("briefing"),
        "a missing briefings table was read as 'not written yet'"
    );

    // A row whose column is not the type the statement asked for: the rows read before it
    // are kept, and the failure is reported instead of silently shortening the list.
    //
    // The order matters and is the query's own (`weight DESC`): rusqlite stops iterating at
    // the row that would not decode, so the good row is given the heavier weight and is read
    // first — which is exactly the situation this promises to handle: a graph that draws some
    // of its lines and says the rest could not be read.
    let broken = {
        let conn = Connection::open_in_memory().expect("an in-memory archive");
        conn.execute_batch(
            "CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL, label TEXT);
             INSERT INTO edges(a, b, weight, label) VALUES(1, 2, 1.0, x'00'), (1, 3, 2.0, 'y');",
        )
        .expect("a table that lies about one column");
        Archive {
            conn,
            path: PathBuf::from(":memory:"),
            read_error: RefCell::new(None),
        }
    };
    let edges = broken.edges(&[1, 2, 3], 10);
    let kept: Vec<(i64, i64, f64)> =
        edges.iter().map(|edge| (edge.a, edge.b, edge.weight)).collect();
    assert_eq!(kept, vec![(1, 3, 2.0)], "the rows that decoded were thrown away with the bad one");
    let error = broken.last_error().expect("a row that would not decode was dropped silently");
    assert_eq!(error.read, "edges");
    assert!(
        error.message.to_lowercase().contains("invalid column type"),
        "the row's reason is not SQLite's: {error}"
    );
}

/// Rule 5: the briefing is a read, and reading it writes nothing — not a byte of the file,
/// not a journal beside it.
///
/// The bytes are compared, not the timestamps: a `-wal` or `-journal` file is the write a
/// timestamp misses, so the directory is listed too.
#[test]
fn the_briefing_read_writes_nothing() {
    let scratch = Scratch::new("briefing");
    let archive = scratch.archive(|writable| {
        writable
            .execute_batch(
                "INSERT INTO briefings(scope, key, hours, watermark, created, line, bullets,
                     articles, model, ms) VALUES('window', '', 72, 1, '2026-11-21T00:00:00Z',
                     'One sentence.', '[\"a fact\"]', 4, 'test-model', 7);
                 INSERT INTO articles(title, published, relevant, dupe)
                     VALUES('one', '2026-11-21T00:00:00Z', 1, 0);",
            )
            .expect("a briefing");
    });
    let before = std::fs::read(scratch.file()).expect("the archive's bytes");
    let listing = |dir: &PathBuf| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("the scratch directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let files_before = listing(&scratch.dir);

    // Every shape of the read: the row, another window, another scope.
    let briefing = archive.briefing("window", "", 72).expect("the row it was given");
    assert_eq!(briefing.line, "One sentence.");
    assert_eq!(briefing.bullets, vec!["a fact".to_string()]);
    assert!(archive.briefing("window", "", 24).is_none());
    assert!(archive.briefing("country", "DEU", 72).is_none());
    assert_eq!(archive.last_error(), None, "reading a briefing reported a failure");

    let after = std::fs::read(scratch.file()).expect("the archive's bytes");
    assert!(
        before == after,
        "the briefing read changed the archive: {} bytes before, {} after",
        before.len(),
        after.len()
    );
    assert_eq!(listing(&scratch.dir), files_before, "the briefing read left a file behind");
    assert_eq!(
        files_before,
        vec!["atlas.db".to_string()],
        "a plain archive should be one file: {files_before:?}"
    );
}
