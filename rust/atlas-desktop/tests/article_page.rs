//! The Articles page and its reading pane, pinned from the outside.
//!
//! The page is the only place a reader can find a story again, so what it must never
//! get wrong is what it shows and what it hides:
//!
//! * the search box narrows the list by title, summary **or** body, and an empty box
//!   narrows nothing — including a box holding only spaces;
//! * a search that matches nothing returns nothing. The classic bug is the sentinel:
//!   one statement with `LIKE '%' || ? || '%'` and an empty needle becomes `LIKE
//!   '%%'`, which matches every row, so "no results" shows the whole archive instead;
//! * the window is a range read on `published`, honoured on both sides of each of the
//!   three boundaries the rail offers (24 h, 3 d, 7 d);
//! * the reading pane shows the body when a body was fetched and the summary when it
//!   was not, and says which of the two it is showing — `full_text` is the flag, and
//!   a teaser that still carries a description is not a fetched story;
//! * every pin a dot is drawn for points at a story **that the window's own list
//!   holds**, so a click on the map can never open a story the page does not have — and
//!   when the cap on the list leaves that story out, the read tops the list up by key
//!   (`app::reload`) instead of the reader being told the story is gone;
//! * `views::clip` cuts on a character boundary and marks the cut once;
//! * `views::when` renders today as a time and an older day as a date, never panics on
//!   the hostile stamps this window does not write (a panic in the frame closes the
//!   window: the paint runs inside the platform's event callback), and never lets one
//!   such stamp forge a second line inside a row.
//!
//! # How these are pinned
//!
//! The data questions are asked of the library (`Archive::articles`) against a seeded
//! fixture in the temp directory, and again against the live archive read-only. The
//! page questions are asked of the page: one headless `egui` frame, with every painted
//! string collected from the frame's own text shapes — the same technique the crate's
//! own view tests use, so a pane that draws the summary where the body belongs fails
//! here rather than in a screenshot nobody takes.
//!
//! Nothing here writes the archive, opens a socket, or needs a window.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::data::{Archive, Article, Pin};
use atlas_desktop::theme;
use atlas_desktop::views::{self, articles};

use rusqlite::Connection;

/// A window wider than any archive: the assertions that are about the archive's
/// contents must not depend on where the moving edge of "now" falls.
const WIDE: i64 = 24 * 365 * 10;
/// More rows than the page can show, so `LIMIT` is not what shapes a list.
const ALL: i64 = 10_000;
/// The rows one demand reads: `app::reload` asks the archive for 400 pins and 400
/// articles, so 400 is the list a click on a dot has to land in.
const PAGE_LIMIT: i64 = 400;

// -------------------------------------------------------------------- fixtures

/// A fresh directory per test, so two tests never share a database.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "atlas-article-page-{}-{}-{}",
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

/// The tables and columns the article statement names, and nothing else.
const SCHEMA: &str = "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
     CREATE TABLE articles(
         id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
         published TEXT, summary TEXT, content TEXT, place TEXT, iso TEXT, url TEXT,
         full_text INTEGER, relevant INTEGER, dupe INTEGER, lat REAL, lon REAL);";

/// An ISO stamp `seconds` in the past, in the archive's own format.
///
/// Asked of SQLite (`strftime` is UTC, fixed width, `Z`), so a fixture cannot drift from
/// the format the window comparison is written against. Seconds, not hours: a fixture
/// that has to hold more stories than the list's cap inside one window cannot space them
/// an hour apart.
fn iso_seconds_ago(conn: &Connection, seconds: i64) -> String {
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        [format!("-{seconds} seconds")],
        |row| row.get(0),
    )
    .expect("strftime")
}

/// One story in a fixture. The text is owned, because the reader's tests build a
/// body long enough that the list's own clip cannot have produced it.
struct Story {
    title: String,
    summary: String,
    content: String,
    /// The place the archive named, which is what a pin is labelled with.
    place: String,
    /// The coordinate, when the story was located: without one it is on no map, which
    /// is the archive's own way of saying "no place".
    at: Option<(f64, f64)>,
    full_text: i64,
    relevant: i64,
    dupe: i64,
    /// How old the story is. Seconds, not hours: a fixture that holds more stories than
    /// the list's cap inside one window cannot space them an hour apart.
    ago_seconds: i64,
}

/// A fetched story: it carries the body the reader showed.
fn fetched(title: &str, summary: &str, content: &str, hours_ago: i64) -> Story {
    Story {
        title: title.to_string(),
        summary: summary.to_string(),
        content: content.to_string(),
        place: String::new(),
        at: None,
        full_text: 1,
        relevant: 1,
        dupe: 0,
        ago_seconds: hours_ago * 3_600,
    }
}

/// A fetched story the archive located, so it reaches a pin.
fn located(title: &str, place: &str, lat: f64, lon: f64, hours_ago: i64) -> Story {
    Story { place: place.to_string(), at: Some((lat, lon)), ..fetched(title, "s", "c", hours_ago) }
}

/// The same, `seconds` old, for a fixture that has to fit many stories in one window.
fn located_seconds(title: &str, place: &str, lat: f64, lon: f64, seconds: i64) -> Story {
    Story { ago_seconds: seconds, ..located(title, place, lat, lon, 0) }
}

/// The same story, filed as one the archive does not show: it is not relevant, or it is
/// a duplicate of another story.
fn irrelevant(mut story: Story) -> Story {
    story.relevant = 0;
    story
}

fn duplicate(mut story: Story) -> Story {
    story.dupe = 1;
    story
}

/// A story that was not fetched: whatever text it has is the teaser.
fn teaser(title: &str, summary: &str, content: &str, hours_ago: i64) -> Story {
    Story { full_text: 0, ..fetched(title, summary, content, hours_ago) }
}

/// A seeded archive, opened through the code under test.
fn archive_of(tag: &str, stories: &[Story]) -> (PathBuf, Archive) {
    let dir = temp_dir(tag);
    let path = dir.join("atlas.db");
    {
        let conn = Connection::open(&path).expect("a writable fixture to seed");
        conn.execute_batch(SCHEMA).expect("the archive's shape");
        conn.execute("INSERT INTO sources(id, name) VALUES (1, 'Wired')", [])
            .expect("a source");
        for (index, story) in stories.iter().enumerate() {
            conn.execute(
                "INSERT INTO articles(id, title, source_id, published, summary, content,
                     place, iso, url, full_text, relevant, dupe, lat, lon)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, 'DEU', NULL, ?7, ?10, ?11, ?8, ?9)",
                rusqlite::params![
                    index as i64 + 1,
                    story.title,
                    iso_seconds_ago(&conn, story.ago_seconds),
                    story.summary,
                    story.content,
                    story.place,
                    story.full_text,
                    story.at.map(|(lat, _)| lat),
                    story.at.map(|(_, lon)| lon),
                    story.relevant,
                    story.dupe
                ],
            )
            .expect("a story");
        }
    }
    let archive = Archive::open(&path).expect("the fixture, read-only");
    (dir, archive)
}

/// The titles of a list, in the order the page would draw them.
fn titles(stories: &[Article]) -> Vec<String> {
    stories.iter().map(|story| story.title.clone()).collect()
}

// ------------------------------------------------------------------- the page

/// Every string one headless frame of the page painted.
///
/// Two frames, because the first lays the panels out and the second paints them in the
/// space they landed in. `view` is the page's own state, so a test sets the selection
/// and reads what the reader drew.
///
/// `egui 0.37`: `Context::run_ui` hands the callback the *root* `Ui` and panels are shown
/// inside it, which is the shape `app::Atlas` uses. The frame's texture deltas are
/// released before it is dropped — a headless frame has no renderer to upload them to,
/// and `epaint` panics on a `FullOutput` dropped with deltas still in it.
fn painted(view: &mut articles::ArticlesView, stories: &[Article], hours: i64) -> Vec<String> {
    let context = egui::Context::default();
    theme::install(&context);
    let screen = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let mut out = Vec::new();
    for _ in 0..2 {
        let output = context.run_ui(screen.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                articles::show(ui, view, stories, hours);
            });
        });
        out.clear();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        output.drop_without_applying_deltas();
    }
    out
}

fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Vec(items) => {
            for item in items {
                collect(item, out);
            }
        }
        egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
        _ => {}
    }
}

/// The reader's own paragraph: the painted string that *is* the body or the summary.
///
/// The list paints a clipped summary, so a body longer than the list's 240-character
/// cut can only have come from the pane.
fn opened(view: &mut articles::ArticlesView, stories: &[Article]) -> Vec<String> {
    painted(view, stories, 72)
}

// --------------------------------------------------------------- the search box

#[test]
fn the_search_box_finds_a_word_in_the_title_the_summary_or_the_body() {
    // one word in each of the three columns the statement looks at, and one story
    // that shares none of them
    let (dir, archive) = archive_of(
        "search",
        &[
            fetched("Chip export rules", "a ministry statement", "the body of the first story", 2),
            fetched("Ferry timetable", "North Sea routes resume", "the body mentions turbines", 3),
            fetched("Harvest outlook", "wheat yields improve", "a body about grain prices", 4),
            fetched("Council budget", "a vote on Tuesday", "no shared word here at all", 5),
        ],
    );
    let all = archive.articles(72, ALL, "");
    assert_eq!(all.len(), 4, "an empty box is every story in the window");

    // a word only in a title
    let hits = archive.articles(72, ALL, "Harvest");
    assert_eq!(titles(&hits), vec!["Harvest outlook"]);
    // a word only in a summary
    let hits = archive.articles(72, ALL, "ministry");
    assert_eq!(titles(&hits), vec!["Chip export rules"]);
    // a word only in a body
    let hits = archive.articles(72, ALL, "grain");
    assert_eq!(titles(&hits), vec!["Harvest outlook"]);
    // and the case of the needle is not the reader's problem: SQLite's LIKE folds
    // ASCII, so a lower-case box finds a capitalised title
    assert_eq!(titles(&archive.articles(72, ALL, "harvest")), vec!["Harvest outlook"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_empty_box_and_a_box_of_spaces_both_filter_nothing() {
    let (dir, archive) = archive_of(
        "empty-box",
        &[
            fetched("One", "a summary", "a body", 2),
            fetched("Two", "another summary", "another body", 3),
        ],
    );
    for needle in ["", " ", "   ", "\t"] {
        assert_eq!(
            archive.articles(72, ALL, needle).len(),
            2,
            "{needle:?} should be an empty box, not a search"
        );
    }
    // and the limit still bounds it: an empty box is not "no limit"
    assert_eq!(archive.articles(72, 1, "").len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_search_that_matches_nothing_returns_nothing() {
    let (dir, archive) = archive_of(
        "no-hits",
        &[
            fetched("Chip export rules", "the ministry said", "a body about chips", 2),
            fetched("Ferry timetable", "North Sea routes", "a body about ferries", 3),
        ],
    );
    // the sentinel bug: `LIKE '%%'` would return the whole archive here
    assert_eq!(archive.articles(72, ALL, "zzzqqq").len(), 0, "a miss returned rows");
    assert_eq!(archive.articles(72, ALL, "chips and ferries").len(), 0, "a phrase is a miss");
    // a needle is text, not a pattern: `%` finds the rows holding a percent sign, not
    // every row, and `_` finds the rows holding an underscore
    assert_eq!(archive.articles(72, ALL, "%").len(), 0, "a percent became a wildcard");
    assert_eq!(archive.articles(72, ALL, "_").len(), 0, "an underscore became a wildcard");
    let (dir2, percent) = archive_of(
        "wildcards",
        &[
            fetched("Rates up 50% this year", "a summary", "a body", 2),
            fetched("a_b in the title", "a summary", "a body", 3),
        ],
    );
    assert_eq!(titles(&percent.articles(72, ALL, "50%")), vec!["Rates up 50% this year"]);
    assert_eq!(titles(&percent.articles(72, ALL, "%")), vec!["Rates up 50% this year"]);
    assert_eq!(titles(&percent.articles(72, ALL, "_")), vec!["a_b in the title"]);
    // a backslash is a literal too, not an escape that swallows itself
    assert_eq!(percent.articles(72, ALL, "\\").len(), 0);
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&dir2).ok();
}

// ------------------------------------------------------------------- the window

#[test]
fn the_window_is_honoured_on_both_sides_of_every_boundary() {
    // one story on each side of each of the three boundaries the rail offers
    let (dir, archive) = archive_of(
        "window",
        &[
            fetched("two hours", "s", "c", 2),
            fetched("thirty hours", "s", "c", 30),
            fetched("eighty hours", "s", "c", 80),
            fetched("two hundred hours", "s", "c", 200),
        ],
    );
    assert_eq!(
        titles(&archive.articles(24, ALL, "")),
        vec!["two hours"],
        "24 h keeps the story inside it and drops the one just outside"
    );
    assert_eq!(
        titles(&archive.articles(72, ALL, "")),
        vec!["two hours", "thirty hours"],
        "3 d reaches back past 30 h and stops before 80 h"
    );
    assert_eq!(
        titles(&archive.articles(168, ALL, "")),
        vec!["two hours", "thirty hours", "eighty hours"],
        "7 d reaches back past 80 h and stops before 200 h"
    );
    // and the story outside every window is only in a wider one: the bound moves
    assert_eq!(archive.articles(240, ALL, "").len(), 4);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_page_says_which_window_and_how_many_stories() {
    let (dir, archive) = archive_of(
        "heading",
        &[fetched("One", "s", "c", 2), fetched("Two", "s", "c", 3)],
    );
    let stories = archive.articles(72, ALL, "");
    let mut view = articles::ArticlesView::default();
    let painted = painted(&mut view, &stories, 72);
    assert!(
        painted.iter().any(|text| text == "2 stories in the last 72 h, newest first"),
        "the heading does not say what it is showing: {painted:?}"
    );
    // the newest of the two comes first, and both are on the list
    for title in ["One", "Two"] {
        assert!(painted.iter().any(|text| text == title), "{title:?} was not painted");
    }
    assert_eq!(titles(&stories), vec!["One", "Two"]);
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------- the reader

#[test]
fn the_reader_shows_the_body_that_was_fetched_and_the_teaser_that_was_not() {
    // The list clips a summary to 240 characters, so a string longer than that can
    // only have reached the frame from the pane.
    let body = "the fetched body ".repeat(30) + "end of the body";
    let summary = "the fetched summary ".repeat(30) + "end of the summary";
    let teaser_summary = "the teaser summary ".repeat(30) + "end of the teaser";
    let teaser_body = "a description the feed gave ".repeat(30) + "end";
    let (dir, archive) = archive_of(
        "reader",
        &[
            fetched("Fetched story", &summary, &body, 2),
            teaser("Teaser story", &teaser_summary, &teaser_body, 3),
            teaser("Bare teaser", "only a summary", "", 4),
            teaser("No text at all", "", "", 5),
        ],
    );
    let stories = archive.articles(72, ALL, "");
    assert_eq!(stories.len(), 4);

    // a fetched story: the full body, and no "teaser only" note
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(1) };
    let painted = opened(&mut view, &stories);
    assert!(painted.iter().any(|text| *text == body), "the pane did not paint the body");
    assert!(
        painted.iter().all(|text| !text.contains("Teaser only")),
        "a fetched story was called a teaser"
    );

    // a teaser that still carries a description: the description, the note, and never
    // the summary — the pane's text is what the archive kept, not a stand-in
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(2) };
    let painted = opened(&mut view, &stories);
    assert!(painted.iter().any(|text| *text == teaser_body), "the teaser body was not painted");
    assert!(
        painted.iter().any(|text| text.contains("Teaser only")),
        "a teaser was not marked as one: {painted:?}"
    );
    assert!(
        painted.iter().all(|text| *text != teaser_summary),
        "the pane painted the summary where the stored description belongs"
    );

    // a teaser with no body: the summary is the body, and it is still a teaser
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(3) };
    let painted = opened(&mut view, &stories);
    assert!(painted.iter().any(|text| *text == "only a summary"));
    assert!(painted.iter().any(|text| text.contains("Teaser only")));

    // and a story with neither says so instead of drawing an empty pane
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(4) };
    let painted = opened(&mut view, &stories);
    assert!(
        painted.iter().any(|text| text.contains("No text was stored")),
        "an empty story painted nothing to explain itself: {painted:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_page_says_what_it_wants_when_nothing_is_selected() {
    let (dir, archive) = archive_of("states", &[fetched("One", "s", "c", 2)]);
    let stories = archive.articles(72, ALL, "");
    let mut view = articles::ArticlesView::default();
    let idle = painted(&mut view, &stories, 72);
    assert!(
        idle.iter().any(|text| text.contains("Choose a story on the left")),
        "an empty reader said nothing: {idle:?}"
    );
    // a selection that is no longer in the window is a sentence, not a blank pane
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(999) };
    let stale = painted(&mut view, &stories, 72);
    assert!(
        stale.iter().any(|text| text.contains("no longer in the window")),
        "a stale selection painted nothing: {stale:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------------- clip

#[test]
fn clip_never_splits_a_character_and_marks_the_cut_once() {
    // a text shorter than the limit is returned untouched, with no ellipsis
    assert_eq!(views::clip("Kyiv", 24), "Kyiv");
    assert_eq!(views::clip(&"x".repeat(24), 24), "x".repeat(24), "the limit itself is not a cut");

    // multi-byte characters: one char each, four bytes each — a byte-sliced
    // implementation panics here or returns a broken string
    let globe = "🌍".repeat(20);
    let cut = views::clip(&globe, 6);
    assert_eq!(cut.chars().count(), 7, "{cut:?} is six glyphs and one ellipsis");
    assert!(cut.ends_with('…'));
    assert_eq!(cut.trim_end_matches('…'), globe.chars().take(6).collect::<String>());

    // every cut is a character prefix of its input, and ends with exactly one ellipsis
    for text in [
        "a body with spaces in it and more words than the box holds",
        "Київські новини про зерно та експорт",
        "漢字の見出しがここにあります、とても長い見出しです",
        "onewordwithoutanyspacesatallthatgoesonandonandon",
        "  leading spaces and then a long tail of words here  ",
        "\u{0}a NUL and then a long tail of words to cut through",
    ] {
        for limit in [0, 1, 2, 5, 24, 40] {
            let out = views::clip(text, limit);
            let cut = out.ends_with('…');
            let kept = if cut { &out[..out.len() - '…'.len_utf8()] } else { out.as_str() };
            assert!(text.starts_with(kept), "{out:?} is not a prefix of {text:?}");
            assert!(
                out.chars().count() <= limit.max(1) + 1,
                "{out:?} is longer than {limit} characters plus an ellipsis"
            );
            if text.chars().count() <= limit {
                assert_eq!(out, text, "a text inside the limit was cut");
            } else {
                assert!(cut, "{text:?} at {limit} was cut with no ellipsis");
                assert_eq!(
                    out.matches('…').count(),
                    1,
                    "{out:?} marks the cut more than once"
                );
            }
        }
    }

    // a cut falls at a space when there is one in reach, so a word is not halved
    let out = views::clip("the ministry said on Tuesday that the vote would wait", 20);
    assert_eq!(out, "the ministry said…", "the cut ignored the word boundary");
    // and a single long word has no boundary to fall back on: it is cut hard
    assert_eq!(views::clip(&"w".repeat(40), 10), format!("{}…", "w".repeat(10)));
}

// -------------------------------------------------------------------- when

#[test]
fn when_renders_a_time_for_today_and_a_day_for_anything_older() {
    // today, asked of the same UTC clock the window bound is written against
    let conn = Connection::open_in_memory().expect("an in-memory clock");
    let today: String = conn
        .query_row("SELECT strftime('%Y-%m-%dT09:05:00Z', 'now')", [], |row| row.get(0))
        .expect("a stamp for today");
    assert_eq!(views::when(&today), "09:05", "today is a time, not a date");
    // yesterday-ish: a fixed past day is a day and a month, and nothing else
    assert_eq!(views::when("2026-03-04T09:05:00Z"), "04 Mar");
    assert_eq!(views::when("2026-12-31T23:59:59Z"), "31 Dec");
    // a stamp with no seconds is still a stamp
    assert_eq!(views::when("2026-03-04T09:05"), "04 Mar");
    // a blank stamp is a blank label, and a label is never longer than one line's worth
    assert_eq!(views::when(""), "");
    assert_eq!(views::when("   "), "");
}

#[test]
fn when_never_panics_on_a_stamp_it_cannot_read() {
    // The severe case: the old version sliced `&day[8..10]` after only a byte count,
    // so a multi-byte character in a date position panicked while slicing — inside the
    // frame's event callback, which closes the window.
    let hostile = [
        "2026-11€T10:25:12Z",
        "2026-1€-21T10:25:12Z",
        "2026-11-2€T10:25:12Z",
        "2026-11-21\u{202e}0:25:12Z",
        "2026-11-21T10:25:12Z\n",
        "\u{202e}2026-11-21T10:25:12Z",
        "not a date at all, but long enough",
        "2026-ab-21T10:25:12Z",
        "20XX-11-21T10:25:12Z",
        "2026-11-21",
        "2026-11-21T10:25",
        "2026-11-21102512Z",
        "ab\ncd",
        "a\rb\u{0}c",
        "2026-11-21\n10:25:12Z",
        "\u{0}\u{1}\u{2}",
        "🌍🌍🌍🌍🌍🌍🌍🌍🌍🌍",
        &"x".repeat(20_000),
        &"ä".repeat(200),
        &"2026-11-21T10:25:12Z".repeat(200),
    ];
    for stamp in hostile {
        let label = views::when(stamp);
        assert!(
            label.chars().count() <= 25,
            "{:?} produced a {}-character label",
            &stamp[..stamp.len().min(40)],
            label.chars().count()
        );
        // A label sits inside a *row*: a newline or a carriage return left in it would
        // forge a second line, and the reader would read a second story.
        assert!(
            !label.chars().any(char::is_control),
            "{stamp:?} produced a label with a control character: {label:?}"
        );
        assert!(!label.contains('\n') && !label.contains('\r'), "{label:?} is more than one line");
        // a stamp with something to say gets a label; one holding nothing but control
        // characters has nothing to say, and says nothing
        if stamp.chars().any(|ch| !ch.is_control() && !ch.is_whitespace()) {
            assert!(!label.trim().is_empty(), "{stamp:?} produced no label at all");
        }
    }
    // the fallback is the stamp itself, clipped — not invented, not dropped, and one line
    assert_eq!(views::when("not a date at all, but long enough"),
        "not a date at all, but…");
    assert_eq!(views::when("ab\ncd"), "ab cd", "a newline forged a second line");
    // a stamp whose date digits are missing is not formatted as a date
    assert_eq!(views::when("2026-XX-21T10:25:12Z"), "2026-XX-21T10:25:12Z");
}

// ------------------------------------------------------------ the live archive

/// The checkout's archive: `rust/atlas-desktop` sits two directories below the root.
fn archive_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/atlas.db")
}

/// The live archive, or `None` on a checkout without one (a fresh clone, a CI box),
/// with a line saying so: the fixture tests above carry the structural assertions.
fn live() -> Option<Archive> {
    let path = archive_path();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some(archive),
        Err(problem) => {
            println!("skipped: {problem}");
            None
        }
    }
}

#[test]
fn the_real_archive_answers_the_same_questions() {
    let Some(archive) = live() else { return };
    // the content assertions use the widest window, so they do not rot as the archive
    // ages out of the rail's three; the three themselves are checked by nesting
    let all = archive.articles(WIDE, ALL, "");
    assert!(!all.is_empty(), "the archive holds no story at all");
    let window = archive.articles(72, ALL, "");

    // (a) an empty box is every story in the window, and every older window is inside
    // the wider one: the three choices are a narrowing, not three different lists
    let day = archive.articles(24, ALL, "");
    let week = archive.articles(168, ALL, "");
    assert!(day.len() <= window.len() && window.len() <= week.len(), "the windows do not nest");
    let ids = |stories: &[Article]| stories.iter().map(|story| story.id).collect::<Vec<i64>>();
    let (day, window, week) = (ids(&day), ids(&window), ids(&week));
    assert!(day.iter().all(|id| window.contains(id)), "24 h is not inside 3 d");
    assert!(window.iter().all(|id| week.contains(id)), "3 d is not inside 7 d");

    // (b) a word the archive does not hold returns nothing — the bug this pins is a
    // `LIKE '%%'` sentinel answering with the whole window
    assert_eq!(archive.articles(WIDE, ALL, "zzzqqq").len(), 0, "a miss answered with rows");

    // (a) and every hit really holds the needle, in one of the three columns
    let hits = archive.articles(WIDE, ALL, "chip");
    assert!(!hits.is_empty(), "the archive holds no story about chips");
    for story in &hits {
        let haystack = format!("{} {} {}", story.title, story.summary, story.content)
            .to_lowercase();
        assert!(haystack.contains("chip"), "{:?} does not hold the needle", story.title);
    }
    println!(
        "article page: {} stories in 3 d, {} in 24 h, {} in 7 d, {} hold \"chip\"",
        window.len(),
        day.len(),
        week.len(),
        hits.len()
    );

    // (d) where a body came from: every fetched story carries one, and a story that
    // was not fetched is still distinguishable — `full_text` is the flag the pane's
    // note reads, and it is not the same question as "is there text"
    let fetched: Vec<&Article> = all.iter().filter(|story| story.full_text).collect();
    let teasers: Vec<&Article> = all.iter().filter(|story| !story.full_text).collect();
    assert!(!fetched.is_empty() && !teasers.is_empty(), "one of the two cases is missing");
    for story in &fetched {
        assert!(!story.content.is_empty(), "{:?} says it was fetched and has no body", story.title);
    }
    let teaser_with_body = teasers.iter().filter(|story| !story.content.is_empty()).count();
    let empty = all
        .iter()
        .filter(|story| story.content.is_empty() && story.summary.is_empty())
        .count();
    println!(
        "article page: {} fetched, {} teasers ({} with a description), {} with no text",
        fetched.len(),
        teasers.len(),
        teaser_with_body,
        empty
    );
    // a story is either fetched with a body or it is a teaser: no third state can
    // reach the pane without one of the two sentences
    for story in &all {
        assert!(
            !(story.full_text && story.content.is_empty()),
            "a fetched story with no body would show the summary and call it the body"
        );
    }
}


// ------------------------------------------------------- a dot and its story

#[test]
fn a_pin_points_at_the_newest_story_at_its_place() {
    // Two stories at one place, so the pin has a choice to make, and one place with a
    // single story. Ids are the fixture's, so "the newest" is checkable exactly.
    let (dir, archive) = archive_of(
        "pin-article",
        &[
            located("the older Berlin story", "Berlin", 52.52, 13.40, 5),
            located("the newer Berlin story", "Berlin", 52.52, 13.40, 2),
            located("the Kyiv story", "Kyiv", 50.45, 30.52, 3),
        ],
    );
    let stories = archive.articles(72, ALL, "");
    let newest: Vec<i64> = stories.iter().map(|story| story.id).collect();
    assert_eq!(newest, vec![2, 3, 1], "newest first, and the ids are the fixture's");

    let pins = archive.pins(72, ALL);
    assert_eq!(pins.len(), 2);
    for pin in &pins {
        assert!(pin.article > 0, "{} points at no story at all", pin.place);
        assert!(
            newest.contains(&pin.article),
            "{} points at story {}, which the list does not hold",
            pin.place,
            pin.article
        );
    }
    // the Berlin pin (both stories) points at the newer of the two, not the older
    assert_eq!(pins[0].count, 2, "Berlin is the heavier pin");
    assert_eq!(pins[0].article, 2, "a dot opens the newest story at its place");
    assert_eq!(pins[1].article, 3);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_pin_of_the_real_archive_points_at_a_story_its_window_holds() {
    // The complaint this answers: a click on a dot must never land on "that story is no
    // longer in the window". A pin is a *place*, and the dot needs a destination, so the
    // id it carries has to be in the same read the page was drawn from.
    let Some(archive) = live() else { return };
    let mut checked = 0usize;
    let mut most = 0usize;
    for hours in [24, 72, 168, WIDE] {
        let pins = archive.pins(hours, PAGE_LIMIT);
        let stories = archive.articles(hours, PAGE_LIMIT, "");
        let ids: Vec<i64> = stories.iter().map(|story| story.id).collect();
        for pin in &pins {
            assert!(
                pin.article > 0,
                "{} ({} stories) points at no story: article {}",
                pin.place,
                pin.count,
                pin.article
            );
            assert!(
                ids.contains(&pin.article),
                "{} points at story {}, which the {hours} h list of {} rows does not hold",
                pin.place,
                pin.article,
                ids.len()
            );
            checked += 1;
        }
        most = most.max(pins.len());
        println!(
            "article page: {hours} h — {} pins, {} stories, every pin's story in the list",
            pins.len(),
            stories.len()
        );
    }
    assert!(checked > 0, "no pin was checked: the archive holds no located story");
    println!("article page: {checked} pins over four windows, {most} in the busiest one");
}

// ------------------------------------------------- a list that has to top itself up

/// One frame of the whole window: the rail, the status row and the page, drawn by the
/// shipped `Atlas::draw`.
///
/// Nothing here is a second implementation of the read: `Atlas::with_archive` is the
/// constructor the app uses, so the list these tests read is the list `app::reload`
/// built — including the top-up that puts a pin's story into it.
///
/// `height` is the viewport: the page scrolls, and a row outside the clip rect is not
/// painted, so a test that reads "is this story in the list" off the painted strings
/// needs a window tall enough to hold the list.
fn drive(atlas: &mut Atlas, height: f32) -> Vec<String> {
    let context = egui::Context::default();
    theme::install(&context);
    let screen = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1440.0, height),
        )),
        ..Default::default()
    };
    let mut out = Vec::new();
    for _ in 0..2 {
        let output = context.run_ui(screen.clone(), |ui| atlas.draw(ui));
        out.clear();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        output.drop_without_applying_deltas();
    }
    out
}

/// The list `app::reload` builds from a capped read: every pin story the cap left out,
/// read by key, and the whole thing newest-first again.
///
/// Transcribed from `src/app.rs::reload`. It is here to show the reader both sides of
/// the gap on a list small enough to read; the rule itself is driven through the shipped
/// path in [`the_shipped_list_tops_up_a_pins_story_beyond_the_cap`], so if that rule
/// changes, that test is the one that must still pass.
fn topped_up(archive: &Archive, mut articles: Vec<Article>, pins: &[Pin]) -> Vec<Article> {
    let held: std::collections::HashSet<i64> = articles.iter().map(|row| row.id).collect();
    let missing: Vec<i64> = pins
        .iter()
        .map(|pin| pin.article)
        .filter(|id| *id > 0 && !held.contains(id))
        .collect();
    if !missing.is_empty() {
        articles.extend(archive.articles_by_ids(&missing));
        articles.sort_by(|left, right| {
            (right.published.as_str(), right.id).cmp(&(left.published.as_str(), left.id))
        });
    }
    articles
}

#[test]
fn the_shipped_list_tops_up_a_pins_story_beyond_the_cap() {
    // 402 stories in one window, each at its own place, and the place a dot points at is
    // the oldest of them: its story is newer than nothing the list holds, so a 400-row
    // read never sees it and a click on that dot used to open "That story is no longer
    // in the window."
    let mut stories: Vec<Story> =
        vec![located_seconds("the story a dot points at", "Aaa target", 5.0, 5.0, 40_000)];
    for index in 0..401 {
        stories.push(located_seconds(
            &format!("story {index:03}"),
            &format!("place {index:03}"),
            -40.0 + (index % 40) as f64 * 2.0,
            -170.0 + (index / 40) as f64 * 15.0,
            (index as i64 + 1) * 60,
        ));
    }
    let (dir, archive) = archive_of("over-the-cap", &stories);
    let all = archive.articles(WIDE, ALL, "");
    assert_eq!(all.len(), 402, "the fixture is over the cap");
    let target = all.last().expect("the oldest story").clone();
    assert_eq!(target.title, "the story a dot points at");
    assert_eq!(target.id, 1, "the fixture's first row is the oldest");

    // the gap: the capped read, and the dot that points outside it
    let capped = archive.articles(72, PAGE_LIMIT, "");
    assert_eq!(capped.len(), PAGE_LIMIT as usize, "the list is capped");
    assert!(
        !capped.iter().any(|story| story.id == target.id),
        "the capped list already held the story: this fixture cannot witness the top-up"
    );
    let pins = archive.pins(72, PAGE_LIMIT);
    let pin = pins
        .iter()
        .find(|pin| pin.place == "Aaa target")
        .expect("the place is on the map");
    assert_eq!(pin.article, target.id, "the dot points at the oldest story at its place");

    // before the fix: the page drawn from the capped read, with that dot's story selected,
    // is the complaint itself — the reader is told the story is no longer in the window
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(target.id) };
    let before = painted(&mut view, &capped, 72);
    assert!(
        before.iter().any(|text| text.contains("no longer in the window")),
        "the capped list did not produce the complaint: {before:?}"
    );

    // the shipped read: the window is opened on this archive and the Articles page drawn
    let mut atlas = Atlas::with_archive(dir.join("atlas.db"));
    atlas.set_page(Page::Articles);
    // a window tall enough for 401 rows: the assertion below is about the *list*, and a
    // row the clip rect cuts away is not painted at all
    let painted = drive(&mut atlas, 40_000.0);
    assert!(
        painted.iter().any(|text| text == "401 stories in the last 72 h, newest first"),
        "the list did not grow by the one story it was missing: {painted:?}"
    );
    assert!(
        painted.iter().any(|text| text == &target.title),
        "the list never painted the story the dot points at"
    );
    assert!(
        painted.iter().all(|text| !text.contains("no longer in the window")),
        "the page said the story was gone"
    );
    // every row of the topped-up list was painted: 400 stories of the window plus the one
    // the dot points at, which the capped read never returned
    let rows = painted
        .iter()
        .filter(|text| text.starts_with("story ") || *text == &target.title)
        .count();
    assert_eq!(
        rows,
        PAGE_LIMIT as usize + 1,
        "the painted list is not 400 rows plus the dot's story"
    );
    println!(
        "article page: capped read {} of {} stories, {} rows painted, the dot's story {} in it",
        capped.len(),
        all.len(),
        rows,
        target.id
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_reader_opens_a_pins_story_only_once_the_list_holds_it() {
    // five stories, a list capped at two, and a dot pointing at the third-newest: the
    // reader's half of the same gap, on a list small enough to read both ways
    let (dir, archive) = archive_of(
        "reader-gap",
        &[
            located("the first story", "first", 1.0, 1.0, 5),
            located("the second story", "second", 2.0, 2.0, 4),
            located("the third story", "third", 3.0, 3.0, 3),
            located("the fourth story", "fourth", 4.0, 4.0, 2),
            located("the fifth story", "fifth", 5.0, 5.0, 1),
        ],
    );
    let pins = archive.pins(72, ALL);
    let pin = pins.iter().find(|pin| pin.place == "third").expect("the third place");
    let capped = archive.articles(72, 2, "");
    assert_eq!(titles(&capped), vec!["the fifth story", "the fourth story"]);
    assert!(
        !capped.iter().any(|story| story.id == pin.article),
        "the third-newest is outside a two-row list"
    );

    // without the top-up, the reader is told the story is gone — the complaint, exactly
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(pin.article) };
    let painted = opened(&mut view, &capped);
    assert!(
        painted.iter().any(|text| text.contains("no longer in the window")),
        "the capped list did not report the story as missing: {painted:?}"
    );
    assert!(painted.iter().all(|text| text != "the third story"));

    // with it, the reader gets the story, and the list says it holds three
    let topped = topped_up(&archive, capped, &pins);
    assert_eq!(
        titles(&topped),
        vec![
            "the fifth story",
            "the fourth story",
            "the third story",
            "the second story",
            "the first story",
        ],
        "the top-up appends every pin story the cap left out, and re-sorts newest first"
    );
    let mut view = articles::ArticlesView { needle: String::new(), selected: Some(pin.article) };
    let painted = opened(&mut view, &topped);
    assert!(
        painted.iter().any(|text| text == "the third story"),
        "the reader did not paint the story it was asked for: {painted:?}"
    );
    assert!(
        painted.iter().all(|text| !text.contains("no longer in the window")),
        "the reader still said the story was gone"
    );
    assert!(
        painted.iter().any(|text| text == "5 stories in the last 72 h, newest first"),
        "the heading does not count the stories the top-up added"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_top_up_never_resurrects_an_irrelevant_or_duplicate_story() {
    // A dot's destination is a story the archive shows (`relevant`, not a `dupe`), and the
    // top-up must not become a way around that: reading by key keeps the two filters.
    let (dir, archive) = archive_of(
        "top-up-filters",
        &[
            located("kept", "kept place", 10.0, 10.0, 1),
            irrelevant(located("not relevant", "hidden place", 11.0, 11.0, 2)),
            duplicate(located("a duplicate", "dupe place", 12.0, 12.0, 3)),
        ],
    );
    assert_eq!(archive.articles(WIDE, ALL, "").len(), 1, "the fixture hides two stories");
    assert_eq!(titles(&archive.articles_by_ids(&[1])), vec!["kept"]);
    assert!(
        archive.articles_by_ids(&[2]).is_empty(),
        "an irrelevant story was read by key"
    );
    assert!(archive.articles_by_ids(&[3]).is_empty(), "a duplicate was read by key");
    // duplicate keys are read once, and a key that is not one is dropped
    assert_eq!(archive.articles_by_ids(&[1, 1, 1, 0, -4]).len(), 1);
    assert!(archive.articles_by_ids(&[]).is_empty());
    // the parameter list is bounded: more keys than SQLite would take is not an error
    assert_eq!(archive.articles_by_ids(&(1..=700).collect::<Vec<i64>>()).len(), 1);

    // and the top-up itself cannot widen the rule: a pin whose id is hidden adds nothing
    for article in [2_i64, 3] {
        let ghost = Pin { place: String::new(), lat: 0.0, lon: 0.0, count: 1, article };
        assert!(
            topped_up(&archive, Vec::new(), &[ghost]).is_empty(),
            "the top-up resurrected story {article}, which the archive hides"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
