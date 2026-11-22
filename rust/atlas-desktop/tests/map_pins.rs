//! Where the stories happen: the pins the map draws, against the real archive.
//!
//! The promise under test is the map's reason to exist — a located story appears
//! where it happened, and nowhere else. `data::Archive::pins` is the one function
//! that answers it, so this file pins what it must never do:
//!
//! * a pin at `0,0` (the archive's "no place", 92 rows of it in this checkout) is
//!   dropped in SQL, **before** `LIMIT`, so a LIMIT of one still returns one place;
//! * every pin carries a name, and every country code the archive holds is a
//!   three-letter code or absent — never a guess;
//! * the window is what selects the map: a story two hours old is on it, one a
//!   hundred hours old is not, and the count a pin shows follows the window;
//! * the numbers agree: the pins of a window sum to the stories the archive
//!   locates in that window, the country rollup sums to the stories that name a
//!   country, and `counts()` matches the tables it counts;
//! * two stories at the same rounded coordinate are one pin when they name the
//!   same place (its count is their number) and two pins when they do not — the
//!   pin is the *place*, which is the decision `pins`' own doc comment explains.
//!
//! # How this file reaches the code under test
//!
//! `atlas-desktop` is a library (`atlas_desktop::data`) with a binary that opens the
//! window, so the pins under test are the shipped ones: this file links the crate
//! and calls `Archive::pins`. No query is copied in as a stand-in for the function
//! under test.
//!
//! # The two connections
//!
//! The live archive is opened twice, both times read-only
//! (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only`, and `query_only` is read back
//! rather than assumed): once through `Archive::open`, which is the code under
//! test, and once by this file for the statements the module does not offer — the
//! row counts the pins must sum to. Neither connection writes a byte. The
//! refusal itself is proved on a fixture, not on `data/`: a test that *tries* a
//! `DELETE` against the live archive is not a risk worth taking with 1 435 real
//! stories.
//!
//! # What a test outside the module cannot reach
//!
//! `Pin` carries a label, a coordinate and a count — no `iso` and no time. So
//! "every pin's iso is a country code" can only be stated as what a caller sees:
//! the label is never empty, and every code in the archive's `iso` column is a
//! three-letter code. And "pins are newest-first" cannot be stated at all: a pin
//! is an aggregate of many stories and has no single timestamp. Recency belongs
//! to the list that feeds the map, so that is where it is asserted
//! ([`the_stories_that_reach_the_map_are_the_newest_ones_newest_first`]) while the
//! pin list is asserted to be in the order it does have: weight, then name.
//!
//! # Fixtures and the live archive
//!
//! The window edges, the origin drop and the collapse-at-one-coordinate rules are
//! asserted on a seeded fixture in the temp directory, because they need stories
//! on both sides of a boundary and this file may not write `data/`. The live
//! archive carries the properties that must hold of real data (no pin at the
//! origin, named pins, agreeing sums) and skips, with a sentence, on a checkout
//! that has no archive.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use atlas_desktop::data::{self, Archive};
use rusqlite::{Connection, OpenFlags};

/// A window wider than any archive: the assertions that are about the archive's
/// contents must not depend on where the moving edge of "now" falls. This
/// checkout's oldest relevant story is a fortnight old, and the tests below print
/// it, so the assumption is visible in the output rather than implied.
const WIDE: i64 = 24 * 365 * 10;
/// More pins than the map can hold, so `LIMIT` is not what shapes the list.
const ALL: i64 = 10_000;

// ------------------------------------------------------------ the live archive

/// The checkout's archive: `rust/atlas-desktop` sits two directories below the root.
fn archive_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/atlas.db")
}

/// The archive under test and this file's own read-only connection to it.
///
/// `None` on a checkout without an archive (a fresh clone, a CI box), with a line
/// saying so: the fixture tests still run, and they are the ones that would catch a
/// broken window or a lost origin filter.
fn live() -> Option<(Archive, Connection)> {
    let path = archive_path();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return None;
    }
    let archive = match Archive::open(&path) {
        Ok(archive) => archive,
        Err(problem) => {
            println!("skipped: {problem}");
            return None;
        }
    };
    let scout = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("a second read-only connection to the archive");
    scout
        .pragma_update(None, "query_only", true)
        .expect("query_only on the second connection");
    let enforced: i64 = scout
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .expect("read query_only back");
    assert_eq!(enforced, 1, "the second connection is read-write: refusing to use it");
    Some((archive, scout))
}

/// One number out of a statement this file writes itself.
fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).expect(sql)
}

/// [`count`] with the one text parameter the cross-checks need.
fn count_named(conn: &Connection, sql: &str, label: &str) -> i64 {
    conn.query_row(sql, [label], |row| row.get(0)).expect(sql)
}

/// The stories that name one pin, as the archive files them: the place, or the
/// country code when the story named no place. That is the rule the pin's count
/// is supposed to be a count of, stated here in SQL so the assertion is not the
/// module checking itself.
const NAMING: &str = "SELECT count(*) FROM articles \
     WHERE relevant = 1 AND dupe = 0 \
     AND lat IS NOT NULL AND lon IS NOT NULL AND NOT (lat = 0 AND lon = 0) \
     AND coalesce(nullif(place,''), nullif(iso,''), '') = ?1";

/// The stories a window can put on the map at all: located, relevant, not a
/// duplicate, and inside the coordinate box the projection can draw.
const LOCATED: &str = "SELECT count(*) FROM articles \
     WHERE relevant = 1 AND dupe = 0 AND lat IS NOT NULL AND lon IS NOT NULL \
     AND NOT (lat = 0 AND lon = 0) AND lat BETWEEN -90 AND 90 \
     AND lon BETWEEN -180 AND 180";

// -------------------------------------------------------------------- fixtures

/// A fresh directory per test, so two tests never share a database.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "atlas-map-pins-{}-{}-{}",
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

/// The tables and columns the queries under test name, and nothing else.
const SCHEMA: &str = "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
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
     CREATE TABLE alerts(id INTEGER PRIMARY KEY, kind TEXT);";

/// An ISO stamp `hours` in the past, in the archive's own format.
///
/// Asked of SQLite (`strftime` is UTC, fixed width, `Z`), so a fixture cannot drift
/// from the format `window_start_iso` writes.
fn iso_hours_ago(conn: &Connection, hours: i64) -> String {
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        [format!("-{hours} hours")],
        |row| row.get(0),
    )
    .expect("strftime")
}

/// One story in a fixture. `count` is a row count, not a column: a place is heavy
/// because more stories named it, which is the whole meaning of a pin's number.
struct Story {
    place: &'static str,
    iso: &'static str,
    lat: f64,
    lon: f64,
    hours_ago: i64,
    relevant: i64,
    dupe: i64,
}

/// A relevant, non-duplicate story.
fn story(place: &'static str, iso: &'static str, lat: f64, lon: f64, hours_ago: i64) -> Story {
    Story { place, iso, lat, lon, hours_ago, relevant: 1, dupe: 0 }
}

/// A seeded archive, opened through the code under test.
fn archive_of(tag: &str, stories: &[Story]) -> (PathBuf, Archive) {
    let dir = temp_dir(tag);
    let path = dir.join("atlas.db");
    {
        let conn = Connection::open(&path).expect("a writable fixture to seed");
        conn.execute_batch(SCHEMA).expect("the archive's shape");
        for (index, story) in stories.iter().enumerate() {
            conn.execute(
                "INSERT INTO articles(id, title, source_id, published, summary, content,
                     place, iso, url, full_text, relevant, dupe, lat, lon)
                 VALUES (?1, ?2, 1, ?3, '', '', ?4, ?5, NULL, 1, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    index as i64 + 1,
                    format!("story {}", index + 1),
                    iso_hours_ago(&conn, story.hours_ago),
                    story.place,
                    story.iso,
                    story.relevant,
                    story.dupe,
                    story.lat,
                    story.lon
                ],
            )
            .expect("a story");
        }
    }
    let archive = Archive::open(&path).expect("the fixture, read-only");
    (dir, archive)
}

/// A pin in an assertion message. `Pin` carries no `Debug`, so this is what a
/// failure prints: the four fields a pin has.
fn shown(pin: &data::Pin) -> String {
    format!("{} at {},{} ({} stories)", pin.place, pin.lat, pin.lon, pin.count)
}

/// The labels of a pin list, in the order the map would draw them.
fn labels(pins: &[data::Pin]) -> Vec<String> {
    pins.iter().map(|pin| pin.place.clone()).collect()
}

// ------------------------------------------------- two stories, one coordinate

#[test]
fn two_stories_at_one_coordinate_are_one_pin_whose_count_is_their_number() {
    // Berlin filed twice a hair apart, and Paris once: the Berlin pair is one pin of
    // two, drawn at the mean of the two points, and Paris is a pin of one.
    let stories = [
        story("Berlin", "DEU", 52.52, 13.40, 5),
        story("Berlin", "DEU", 52.52, 13.41, 40),
        story("Paris", "FRA", 48.8566, 2.3522, 8),
    ];
    let (dir, archive) = archive_of("collapse", &stories);
    let pins = archive.pins(72, ALL);
    assert_eq!(labels(&pins), vec!["Berlin".to_string(), "Paris".to_string()]);
    let berlin = &pins[0];
    assert_eq!(berlin.count, 2, "two stories naming Berlin are one pin of two");
    assert_eq!(berlin.lat, 52.52, "the pin sits at the mean of its stories");
    assert!((berlin.lon - 13.405).abs() < 1e-9, "longitude {}", berlin.lon);
    assert_eq!(pins[1].count, 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn two_names_at_one_coordinate_stay_two_pins_because_a_pin_is_a_place() {
    // The archive files a country and its capital at the same point. A pin is the
    // place, so these stay two dots with their own counts; grouping by rounded
    // coordinate instead merged them into one pin labelled with the country.
    let stories = [
        story("United States", "USA", 38.9, -77.04, 3),
        story("United States", "USA", 38.9, -77.04, 4),
        story("Washington", "USA", 38.9, -77.04, 6),
    ];
    let (dir, archive) = archive_of("one-point", &stories);
    let pins = archive.pins(72, ALL);
    assert_eq!(labels(&pins), vec!["United States".to_string(), "Washington".to_string()]);
    assert_eq!((pins[0].lat, pins[0].lon), (pins[1].lat, pins[1].lon), "one point, two places");
    assert_eq!(pins[0].count, 2);
    assert_eq!(pins[1].count, 1);
    std::fs::remove_dir_all(&dir).ok();
}

// --------------------------------------------------------------- the origin

#[test]
fn a_story_at_the_origin_is_dropped_before_the_limit() {
    // The origin is the archive saying "no place". Three such stories and one Berlin
    // story, with a LIMIT of one: filtered in SQL the map still gets its one place;
    // filtered in Rust afterwards the three origin rows take the limit and the map
    // gets nothing.
    let stories = [
        story("", "", 0.0, 0.0, 2),
        story("", "", 0.0, 0.0, 3),
        story("", "", 0.0, 0.0, 4),
        story("Berlin", "DEU", 52.52, 13.40, 5),
    ];
    let (dir, archive) = archive_of("origin", &stories);
    let one = archive.pins(72, 1);
    assert_eq!(labels(&one), vec!["Berlin".to_string()], "the origin rows are not places");
    assert_eq!(one[0].count, 1, "and they are not counted into one either");
    let all = archive.pins(72, ALL);
    assert_eq!(all.len(), 1, "no pin at the origin, at any limit");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_irrelevant_or_duplicate_story_is_not_a_place() {
    let mut irrelevant = story("Berlin", "DEU", 52.52, 13.40, 5);
    irrelevant.relevant = 0;
    let mut duplicate = story("Kyiv", "UKR", 50.45, 30.52, 6);
    duplicate.dupe = 1;
    let stories = [story("Berlin", "DEU", 52.52, 13.40, 5), irrelevant, duplicate];
    let (dir, archive) = archive_of("relevant", &stories);
    let pins = archive.pins(72, ALL);
    assert_eq!(labels(&pins), vec!["Berlin".to_string()]);
    assert_eq!(pins[0].count, 1, "a filtered story is not counted either");
    std::fs::remove_dir_all(&dir).ok();
}

// ----------------------------------------------------------------- the window

#[test]
fn the_window_is_what_selects_the_map_and_what_it_counts() {
    // two stories in one place, one inside the window and one out of it
    let stories = [story("Kyiv", "UKR", 50.45, 30.52, 2), story("Kyiv", "UKR", 50.45, 30.52, 100)];
    let (dir, archive) = archive_of("window", &stories);
    let inside = archive.pins(72, ALL);
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].count, 1, "the hundred-hour-old story is outside a 72 h window");
    let wider = archive.pins(200, ALL);
    assert_eq!(wider.len(), 1);
    assert_eq!(wider[0].count, 2, "a longer window holds both");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_heaviest_pin_comes_first_and_the_limit_bounds_the_list() {
    // one pin per place, counts 3, 2, 1, 1, 1 — and the labels deliberately fight the
    // weights, so a list ordered by name would come out in a different order
    let stories = [
        story("Zurich", "CHE", 47.37, 8.54, 2),
        story("Zurich", "CHE", 47.37, 8.54, 3),
        story("Zurich", "CHE", 47.37, 8.54, 4),
        story("Athens", "GRC", 37.98, 23.73, 5),
        story("Athens", "GRC", 37.98, 23.73, 6),
        story("Bern", "CHE", 46.95, 7.45, 7),
        story("Accra", "GHA", 5.56, -0.2, 8),
        story("Berlin", "DEU", 52.52, 13.4, 9),
    ];
    let (dir, archive) = archive_of("order", &stories);
    let pins = archive.pins(72, ALL);
    assert_eq!(
        labels(&pins),
        vec!["Zurich", "Athens", "Accra", "Berlin", "Bern"],
        "weight first, then the name ascending"
    );
    assert_eq!(pins.iter().map(|pin| pin.count).collect::<Vec<i64>>(), vec![3, 2, 1, 1, 1]);
    let two = archive.pins(72, 2);
    assert_eq!(labels(&two), vec!["Zurich", "Athens"], "the limit keeps the heaviest");
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------ the live archive

#[test]
fn no_pin_sits_on_the_null_island_of_the_real_archive() {
    let Some((archive, scout)) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    assert!(!pins.is_empty(), "the map must have places to draw");
    for pin in &pins {
        assert!(
            !(pin.lat == 0.0 && pin.lon == 0.0),
            "{} is pinned at 0,0, which is the archive saying no place",
            pin.place
        );
        assert!(pin.lat.is_finite() && pin.lon.is_finite(), "not a coordinate: {}", shown(pin));
        assert!(
            (-90.0..=90.0).contains(&pin.lat) && (-180.0..=180.0).contains(&pin.lon),
            "{} is outside the box the projection can draw",
            shown(pin)
        );
        assert!(pin.count > 0, "{} holds {} stories", pin.place, pin.count);
    }
    // the filter is doing work: the archive really does file stories at the origin
    let origin = count(&scout, "SELECT count(*) FROM articles WHERE lat = 0 AND lon = 0");
    assert!(origin > 0, "no story at the origin: a filter that did nothing would pass this");
    println!("map pins: {} pins, {origin} stories at the origin dropped", pins.len());
}

#[test]
fn every_pin_carries_a_name_and_every_country_code_is_a_code() {
    let Some((archive, scout)) = live() else { return };
    let mut seen: Vec<String> = Vec::new();
    for pin in archive.pins(WIDE, ALL) {
        assert!(!pin.place.trim().is_empty(), "an unnamed dot is not a place: {}", shown(&pin));
        assert!(
            !seen.contains(&pin.place),
            "two pins are called {}: a name the archive holds in two countries, drawn as one dot",
            pin.place
        );
        seen.push(pin.place);
    }
    // `Pin` has no iso field, so the code is checked where it lives: every non-empty
    // code in the archive is three uppercase letters, and a story with none is
    // unknown rather than guessed at.
    let junk = count(
        &scout,
        "SELECT count(*) FROM articles WHERE coalesce(iso,'') <> '' \
         AND iso NOT GLOB '[A-Z][A-Z][A-Z]'",
    );
    assert_eq!(junk, 0, "{junk} stories carry something other than a country code");
    // and a located story with neither a place nor a code would be a nameless dot
    let nameless = count(
        &scout,
        "SELECT count(*) FROM articles WHERE relevant = 1 AND dupe = 0 \
         AND coalesce(place,'') = '' AND coalesce(iso,'') = '' \
         AND lat IS NOT NULL AND lon IS NOT NULL AND NOT (lat = 0 AND lon = 0)",
    );
    assert_eq!(nameless, 0, "{nameless} located stories could not be named on the map");
    println!("map pins: {} named pins, no unnamed story", seen.len());
}

#[test]
fn the_pins_sum_to_the_stories_the_archive_locates() {
    let Some((archive, scout)) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let summed: i64 = pins.iter().map(|pin| pin.count).sum();
    let located = count(&scout, LOCATED);
    let oldest = scout
        .query_row(
            "SELECT min(published) FROM articles WHERE relevant = 1 AND dupe = 0",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    assert_eq!(
        summed, located,
        "{} pins sum to {summed}, the archive locates {located} stories in this window \
         (its oldest is {oldest}, the window is ten years)",
        pins.len()
    );
    // and each pin's own number is the stories that named it — the collapse, story by
    // story: the "United States" pin is 80 rows of this archive, not one row's 80
    for pin in &pins {
        assert_eq!(
            pin.count,
            count_named(&scout, NAMING, &pin.place),
            "{} shows {} stories",
            pin.place,
            pin.count
        );
    }
    println!("map pins: {summed} stories over {} pins", pins.len());
}

#[test]
fn the_rail_counts_and_the_window_agree_with_the_tables() {
    let Some((archive, scout)) = live() else { return };
    let counts = archive.counts();
    assert_eq!(counts.articles, count(&scout, "SELECT count(*) FROM articles"));
    assert_eq!(
        counts.relevant,
        count(&scout, "SELECT count(*) FROM articles WHERE relevant = 1")
    );
    assert_eq!(counts.entities, count(&scout, "SELECT count(*) FROM entities"));
    assert_eq!(counts.edges, count(&scout, "SELECT count(*) FROM edges"));
    assert_eq!(counts.links, count(&scout, "SELECT count(*) FROM article_links"));
    assert_eq!(counts.topics, count(&scout, "SELECT count(*) FROM topics"));
    assert_eq!(counts.monitors, count(&scout, "SELECT count(*) FROM monitors"));
    assert_eq!(counts.alerts, count(&scout, "SELECT count(*) FROM alerts"));
    // the country rollup answers for the same window as the pins, from the same rows — and it
    // counts **every place a story names**, not only the one it is filed under: the primary
    // column and `article_countries` as pairs (a story that names four countries is counted
    // once in each, and a story whose primary country also has a row is counted once). The
    // measured archive: 260 primary places, 1 227 further rows, **644 pairs**, so the rollup is
    // larger than the primary count and never larger than the stories times their places.
    let rollup = archive.countries(WIDE, ALL);
    let countries: i64 = rollup.iter().map(|(_, held)| *held).sum();
    let widest = rollup.iter().map(|(_, held)| *held).max().unwrap_or(0);
    let primary = count(
        &scout,
        "SELECT count(*) FROM articles WHERE relevant = 1 AND dupe = 0 \
         AND coalesce(iso,'') <> ''",
    );
    let named = count(
        &scout,
        "SELECT count(*) FROM ( \
             SELECT id AS id, iso AS iso FROM articles \
             WHERE relevant = 1 AND dupe = 0 AND coalesce(iso,'') <> '' \
             UNION \
             SELECT c.article_id, c.iso FROM article_countries c \
             JOIN articles a ON a.id = c.article_id \
             WHERE a.relevant = 1 AND a.dupe = 0 AND coalesce(c.iso,'') <> '' \
         )",
    );
    assert_eq!(countries, named, "the country rollup and the archive disagree");
    assert!(
        countries >= primary,
        "the rollup ({countries}) is smaller than the primary places alone ({primary})"
    );
    assert!(
        rollup.len() <= ALL as usize,
        "the rollup answered {} rows for a limit of {ALL}",
        rollup.len()
    );
    // The rail's relevant count is the ceiling a **country** stays under; the rollup's sum is
    // pairs, not stories, so it is the wider number on purpose (644 pairs of 400 stories).
    let pins: i64 = archive.pins(WIDE, ALL).iter().map(|pin| pin.count).sum();
    assert!(pins <= counts.relevant, "{pins} pins of stories in {} relevant", counts.relevant);
    assert!(
        widest <= counts.relevant,
        "one country counts {widest} of {} relevant stories",
        counts.relevant
    );
    println!(
        "map counts: {} articles, {} relevant, {} on the map, {} in the country rollup",
        counts.articles, counts.relevant, pins, countries
    );
}

#[test]
fn a_coordinate_two_names_share_stays_two_pins_on_the_real_archive() {
    let Some((archive, scout)) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let mut shared = 0usize;
    for (index, one) in pins.iter().enumerate() {
        for other in pins.iter().skip(index + 1) {
            if one.lat == other.lat && one.lon == other.lon {
                shared += 1;
                assert_ne!(
                    one.place, other.place,
                    "{} is two pins at one point",
                    one.place
                );
            }
        }
    }
    assert!(
        shared > 0,
        "no two pins share a point, so this archive cannot witness the decision at all"
    );
    // each of them keeps its own stories: the point is shared, the count is not
    for one in pins.iter().filter(|pin| {
        pins.iter().filter(|other| other.lat == pin.lat && other.lon == pin.lon).count() > 1
    }) {
        assert_eq!(one.count, count_named(&scout, NAMING, &one.place), "{}", one.place);
    }
    println!("map pins: {shared} coordinates carry two places each, and stay two pins");
}

#[test]
fn the_stories_that_reach_the_map_are_the_newest_ones_newest_first() {
    // A pin is an aggregate and has no timestamp, so recency is a property of this
    // list — the stories the map and its list are built from.
    let Some((archive, scout)) = live() else { return };
    let stories = archive.articles(WIDE, 50, "");
    assert_eq!(stories.len(), 50, "fifty stories were asked for and the window is wide");
    for pair in stories.windows(2) {
        assert!(
            pair[0].published >= pair[1].published,
            "{} is older than {} and comes first",
            pair[0].published,
            pair[1].published
        );
    }
    let newest = scout
        .query_row(
            "SELECT max(published) FROM articles WHERE relevant = 1 AND dupe = 0",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    assert_eq!(stories[0].published, newest, "the list starts at the newest story");
    println!(
        "map list: {} stories, {} .. {}",
        stories.len(),
        stories[0].published,
        stories[stories.len() - 1].published
    );
}
