//! The Topics page's story list: what the archive answers, and what the page does with it.
//!
//! The complaint this file pins down is a dead click: a reader opened a topic and the row
//! opened its terms, and that was all — the stories the topic counts were nowhere, so
//! "click a topic" led nowhere. Two halves are tested, and they are tested apart because
//! they fail apart:
//!
//! * **The read** (`data::Archive::topic_articles`). A temporary archive, opened through
//!   the same `SQLITE_OPEN_READ_ONLY` connection the window uses, is asked for one topic's
//!   stories. What must hold: newest first, only that topic's, only inside the window,
//!   only stories the Articles page would also list (`relevant`, not `dupe`), capped by
//!   the limit — and an archive from before `article_topics` existed answers *nothing*
//!   instead of failing, because a page can print "no stories" and cannot print an error.
//! * **The page** (`views::topics::show_with`). Drawn with no window, three frames per
//!   interaction: the rows appear under the open topic, newest first; a topic the shell
//!   read nothing for draws no list at all (the page may not claim a topic is empty when
//!   it was not told); a click on a row returns that story's id; and Enter on a focused
//!   row returns it too, because the table's keyboard belongs to the whole table.
//!
//! Nothing here reads `data/atlas.db`: the live archive belongs to the user, and every
//! count would move under the test. Every fixture below is built in a temp directory and
//! removed on drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::data::{Archive, Article, Topic};
use atlas_desktop::theme;
use atlas_desktop::views::topics::{self, Stories};

/// The screen every frame is told about: the app's own default window size, so the page
/// lays out as it does on a real 1440×900 screen (`src/main.rs`).
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

/// The window the page is read in, the window `app.rs::reload` uses.
const HOURS: i64 = 72;

// ---------------------------------------------------------------------------
// a temporary archive
// ---------------------------------------------------------------------------

/// A `data/atlas.db` of our own: the tables `data.rs` reads, plus `article_topics`.
///
/// The schema is the shipped one, cut down to the columns [`data::Archive::topic_articles`]
/// touches — `articles` with the `relevant`/`dupe`/`published` triple its window predicate
/// reads, `sources` for the outlet name, and `article_topics` with the primary key the
/// query probes.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

/// Keeps two archives built in the same nanosecond on different test threads apart; a
/// shared directory made `CREATE TABLE` fail once on a parallel run of `tests/smoke.rs`.
static NEXT_ARCHIVE: AtomicU64 = AtomicU64::new(0);

impl TempArchive {
    /// An archive with `article_topics` — the shape a current archive has.
    fn build(hours_ago: &[(i64, i64)]) -> TempArchive {
        TempArchive::with_schema(hours_ago, true)
    }

    /// An archive with no `article_topics` at all: what a reader who has not re-indexed
    /// since topics gained a story list still has on disk.
    fn without_topics(hours_ago: &[(i64, i64)]) -> TempArchive {
        TempArchive::with_schema(hours_ago, false)
    }

    fn with_schema(hours_ago: &[(i64, i64)], with_topics: bool) -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-desktop-topics-{}-{}-{}",
            std::process::id(),
            nanos,
            NEXT_ARCHIVE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("atlas.db");

        let conn = rusqlite::Connection::open(&file).expect("a fresh database");
        conn.execute_batch(
            "CREATE TABLE articles(
                 id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                 published TEXT, summary TEXT, content TEXT, iso TEXT, place TEXT,
                 url TEXT, full_text INTEGER, relevant INTEGER, dupe INTEGER,
                 lat REAL, lon REAL);
             CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE topics(
                 id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER,
                 hits INTEGER, last_hit TEXT);",
        )
        .expect("the schema data.rs reads");
        if with_topics {
            conn.execute_batch(
                "CREATE TABLE article_topics(
                     article_id INTEGER, topic_id INTEGER, score REAL, matched TEXT,
                     PRIMARY KEY(article_id, topic_id));
                 CREATE INDEX idx_at_topic ON article_topics(topic_id);",
            )
            .expect("article_topics");
        }

        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'The Nordic Post'),(2,'Alpine Review')",
            [],
        )
        .expect("two sources");
        conn.execute(
            "INSERT INTO topics(id, name, keywords, active, hits, last_hit) \
             VALUES(1,'Chip supply','chip, semiconductor, export',1,3,?1),\
                   (2,'Harbour labour','harbour, strike, dock',1,2,?1),\
                   (3,'Alpine freight','rail, freight, alpine',0,0,?1)",
            rusqlite::params![stamp(2)],
        )
        .expect("three topics");

        for (id, hours) in hours_ago {
            let (title, source, relevant, dupe, lat, lon) = story(*id);
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', ?3, ?4, ?5, ?6, 'DEU', 'Hamburg', ?7, 1, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    title,
                    source,
                    stamp(*hours),
                    format!("{title} — a summary sentence for the list."),
                    format!("{title}. The body of the story, long enough to fill the pane."),
                    format!("https://example.invalid/{id}"),
                    relevant,
                    dupe,
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        drop(conn);
        TempArchive { dir, file }
    }

    /// File a story under a topic, the way the classifier does.
    fn assign(&self, article: i64, topic: i64, score: f64) {
        let conn = rusqlite::Connection::open(&self.file).expect("the fixture reopens");
        conn.execute(
            "INSERT INTO article_topics(article_id, topic_id, score, matched) VALUES(?1,?2,?3,?4)",
            rusqlite::params![article, topic, score, format!("story {article} matched")],
        )
        .expect("an assignment");
    }

    fn open(&self) -> Archive {
        Archive::open(&self.file).expect("the temporary archive opens read-only")
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Six stories: id, outlet, `relevant`, `dupe`, and a coordinate so the row is complete.
///
/// Story 4 is irrelevant and story 6 is a duplicate of another story. Both are the
/// archive's own reason for not listing a story, and neither belongs in a topic's list:
/// the topic page shows what the Articles page shows.
fn story(id: i64) -> (&'static str, i64, i64, i64, f64, f64) {
    match id {
        1 => ("Chip export rules tighten in Asia", 1, 1, 0, 35.7, 139.7),
        2 => ("A second foundry raises its capacity", 2, 1, 0, 37.6, 127.0),
        3 => ("Wafer supply stays short into autumn", 1, 1, 0, 25.0, 121.5),
        4 => ("Drought reshapes the southern harvest", 2, 0, 0, 40.4, -3.7),
        5 => ("Harbour crane strike ends after nine days", 1, 1, 0, 54.0, 10.0),
        6 => ("Chip export rules tighten in Asia (dupe)", 2, 1, 1, 35.7, 139.7),
        other => panic!("no story {other} in the fixture"),
    }
}

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`), so
/// `published >= window_start_iso(hours)` holds for the rows seeded inside the window.
/// Howard Hinnant's civil-date algorithm again: the crate has no time dependency.
fn stamp(hours_ago: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let at = now - hours_ago * 3600;
    let (days, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The fixture's stories as `(id, hours ago)`: 1 is the newest, 5 the oldest inside the
/// window, and 4, 6 and the far-away 3 sit outside what the page may list.
fn fixture() -> Vec<(i64, i64)> {
    vec![(1, 2), (2, 5), (3, 400), (4, 3), (5, 9), (6, 1)]
}

// ---------------------------------------------------------------------------
// the read
// ---------------------------------------------------------------------------

/// The stories of one topic, newest first — the order the page draws them in, so the
/// newest is what a reader scans first.
#[test]
fn a_topic_lists_its_stories_newest_first() {
    let temp = TempArchive::build(&fixture());
    for story in [1, 2, 5] {
        temp.assign(story, 1, 0.9);
    }

    let archive = temp.open();
    let listed = archive.topic_articles(1, HOURS, 40);
    let ids: Vec<i64> = listed.iter().map(|story| story.id).collect();
    assert_eq!(ids, vec![1, 2, 5], "newest first: {ids:?}");

    // The stamp is part of the row, not only the sort key: it is what the page prints.
    let stamps: Vec<&str> = listed.iter().map(|story| story.published.as_str()).collect();
    let mut sorted = stamps.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(stamps, sorted, "the drawn order is the stamps' order");
    assert_eq!(listed[0].source, "The Nordic Post", "the outlet comes from `sources`");
    assert!(listed[0].title.starts_with("Chip export rules"), "{:?}", listed[0].title);
}

/// One topic's stories, and no other topic's: the join is `article_topics`, so a story
/// filed under topic 2 may not appear under topic 1.
#[test]
fn a_topic_lists_only_its_own_stories() {
    let temp = TempArchive::build(&fixture());
    temp.assign(1, 1, 0.9);
    temp.assign(2, 2, 0.9);
    temp.assign(5, 2, 0.8);

    let archive = temp.open();
    let chip: Vec<i64> = archive
        .topic_articles(1, HOURS, 40)
        .iter()
        .map(|story| story.id)
        .collect();
    let harbour: Vec<i64> = archive
        .topic_articles(2, HOURS, 40)
        .iter()
        .map(|story| story.id)
        .collect();
    assert_eq!(chip, vec![1], "topic 1 got {chip:?}");
    assert_eq!(harbour, vec![2, 5], "topic 2 got {harbour:?}");
}

/// The window is the Articles page's window: a story older than it is not in the list,
/// and neither is one the list would refuse to show (`relevant = 0`, `dupe = 1`).
#[test]
fn the_list_agrees_with_the_articles_page() {
    let temp = TempArchive::build(&fixture());
    for story in [1, 2, 3, 4, 6] {
        temp.assign(story, 1, 0.9);
    }

    let archive = temp.open();
    let ids: Vec<i64> = archive
        .topic_articles(1, HOURS, 40)
        .iter()
        .map(|story| story.id)
        .collect();
    // 3 is 400 h old; 4 is irrelevant; 6 is a duplicate.
    assert_eq!(ids, vec![1, 2], "the window and the gates hold: {ids:?}");

    // The page and the list draw from the same rows: every story in this topic's list is
    // in the Articles list, with the same stamp and outlet, so a reader who opens one from
    // the topic finds the story they were looking at and not a different one.
    let listed = archive.articles(HOURS, 40, "");
    for story in archive.topic_articles(1, HOURS, 40) {
        let same = listed
            .iter()
            .find(|row| row.id == story.id)
            .unwrap_or_else(|| panic!("story {} is not in the Articles list", story.id));
        assert_eq!(same.published, story.published, "story {}", story.id);
        assert_eq!(same.source, story.source, "story {}", story.id);
        assert_eq!(same.title, story.title, "story {}", story.id);
    }
    // 3 is outside the window for both, 4 and 6 are excluded by both: the two lists'
    // windows and gates are one predicate, not two.
    let articles: Vec<i64> = listed.iter().map(|story| story.id).collect();
    assert_eq!(articles, vec![1, 2, 5], "articles: {articles:?}");
}

/// A topic with no story files nothing, and a limit caps what comes back: the newest
/// `limit` stories, not an arbitrary `limit`.
#[test]
fn a_topic_with_no_stories_answers_nothing_and_a_limit_is_respected() {
    let temp = TempArchive::build(&fixture());
    for story in [1, 2, 5] {
        temp.assign(story, 1, 0.9);
    }

    let archive = temp.open();
    assert!(
        archive.topic_articles(3, HOURS, 40).is_empty(),
        "topic 3 has no stories"
    );
    assert!(
        archive.topic_articles(99, HOURS, 40).is_empty(),
        "an id no topic has answers nothing"
    );
    let two: Vec<i64> = archive
        .topic_articles(1, HOURS, 2)
        .iter()
        .map(|story| story.id)
        .collect();
    assert_eq!(two, vec![1, 2], "the newest two: {two:?}");
    assert!(
        archive.topic_articles(1, HOURS, 0).is_empty(),
        "a limit of zero draws nothing rather than everything"
    );
    assert!(
        archive.topic_articles(0, HOURS, 40).is_empty(),
        "topic 0 is not a topic"
    );
}

/// An archive from before `article_topics` existed answers no stories instead of failing:
/// `Archive::open` never panics on a missing table, and neither may the page's read.
#[test]
fn an_archive_without_the_table_answers_nothing_instead_of_failing() {
    let temp = TempArchive::without_topics(&fixture());
    let archive = temp.open();
    assert!(
        archive.topic_articles(1, HOURS, 40).is_empty(),
        "no table, no stories — and no panic"
    );
    // And the page still reads its topics, so the window opens rather than showing an
    // error over the whole rail.
    assert_eq!(archive.topics().len(), 3, "the topics themselves still read");
}

// ---------------------------------------------------------------------------
// the page
// ---------------------------------------------------------------------------

/// One painted frame: every string and where it landed, what clipped it, and the page's
/// own answer.
#[derive(Default)]
struct Painted {
    text: Vec<(String, Rect)>,
    /// The same strings with the scissor they were drawn under, so "outside the rows
    /// region" can be told from "clipped away by it".
    clipped: Vec<(String, Rect, Rect)>,
    rules: Vec<egui::Rangef>,
    asked: Option<i64>,
}

impl Painted {
    /// Where a string was painted, exactly. Exact, not a substring: two topics' stories
    /// can share a word, and a click has to land on the row that was drawn.
    fn rect_of(&self, needle: &str) -> Option<Rect> {
        self.text
            .iter()
            .find(|(text, _)| text == needle)
            .map(|(_, rect)| *rect)
    }

    /// The index of the first string that starts with `start`, in paint order.
    fn at(&self, start: &str) -> Option<usize> {
        self.text
            .iter()
            .position(|(text, _)| text.starts_with(start))
    }

    /// The rect of exactly this string, if it was visible; `None` when it scrolled out of
    /// the region. Exact, because `"Chip supply 1"` is a prefix of `"Chip supply 10"`.
    fn visible_exact(&self, needle: &str) -> Option<Rect> {
        self.clipped
            .iter()
            .find(|(text, rect, clip)| text == needle && rect.intersects(*clip))
            .map(|(_, rect, _)| *rect)
    }

    /// The union of every string's rect: what the page actually painted, which is what a
    /// reader sees when a list has fewer rows than the region can hold.
    fn bbox(&self) -> Rect {
        self.clipped
            .iter()
            .fold(Rect::NOTHING, |all, (_, rect, _)| all.union(*rect))
    }

    fn any(&self, value: &str) -> bool {
        self.text.iter().any(|(text, _)| text == value)
    }

    /// The strings that start with `start` and were actually visible: their own rect meets
    /// the scissor they were drawn under. A row scrolled out of the region is painted with
    /// a rect outside it but is clipped away, which is not the same as a row drawn over the
    /// page.
    fn visible(&self, start: &str) -> Vec<(String, Rect)> {
        self.clipped
            .iter()
            .filter(|(text, rect, clip)| text.starts_with(start) && rect.intersects(*clip))
            .map(|(text, rect, _)| (text.clone(), *rect))
            .collect()
    }
}

/// A context with the app's own style installed, as `Atlas::new` does, so the page is
/// measured with the fonts, spacing and palette it ships with.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

fn input(events: Vec<egui::Event>, time: f64) -> egui::RawInput {
    input_at(SCREEN, events, time)
}

fn input_at(screen: Vec2, events: Vec<egui::Event>, time: f64) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
        time: Some(time),
        events,
        ..Default::default()
    }
}

/// One frame of the page, drawn the way `app.rs` draws it — a panel with the page's own
/// margin, no scrolling wrapper of the test's, because the page scrolls its own rows.
///
/// The frame's `TexturesDelta` is released before the `FullOutput` is dropped:
/// `epaint 0.37` panics in `Drop` when deltas are still unapplied, and there is no
/// renderer here to upload them. `shapes` — all this file asserts on — need no upload.
fn frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    topics: &[Topic],
    stories: Stories<'_>,
) -> Painted {
    frame_in(ctx, input, topics, stories).0
}

/// The same frame, and the box the page was given: the window, less the rail, the status
/// row and the page's own margins — `app.rs`'s skeleton (`src/app.rs:338-514`), built here
/// so a size is a size and not a page-wide guess.
///
/// Nothing opens a window: a `Panel` is a rectangle in a `Ui`, and `Context::run_ui` lays
/// it out with no driver anywhere.
fn frame_in(
    ctx: &egui::Context,
    input: egui::RawInput,
    topics: &[Topic],
    stories: Stories<'_>,
) -> (Painted, Rect) {
    let mut painted = Painted::default();
    let mut page_box = Rect::NOTHING;
    let output = ctx.run_ui(input, |ui| {
        // The rail: fixed width, not resizable, and no page draws in it. Empty on purpose —
        // the only text these frames may contain is the page's, so a painted bbox is the
        // page's and not the shell's.
        egui::Panel::left("rail")
            .exact_size(188.0)
            .resizable(false)
            .show(ui, |ui| {
                let _ = ui;
            });
        // The status row: the same frame the window gives it, at the height the audit
        // measured for it (`38 px at every size`), so the page box below is the app's.
        egui::Panel::bottom("status")
            .exact_size(38.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PAGE)
                    .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE_SOFT))
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ui, |ui| {
                let _ = ui;
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::PAGE)
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                page_box = ui.max_rect();
                painted.asked = topics::show_with(ui, topics, stories);
            });
    });
    for clipped in &output.shapes {
        walk(&clipped.shape, clipped.clip_rect, &mut painted);
    }
    output.drop_without_applying_deltas();
    (painted, page_box)
}

fn walk(shape: &Shape, clip: Rect, painted: &mut Painted) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, clip, painted);
            }
        }
        Shape::Text(text) => {
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            let string = text.galley.text().to_string();
            painted.text.push((string.clone(), rect));
            painted.clipped.push((string, rect, clip));
        }
        Shape::LineSegment { points, .. } => painted
            .rules
            .push(egui::Rangef::new(points[0].x.min(points[1].x), points[0].x.max(points[1].x))),
        _ => {}
    }
}

fn topic(id: i64, name: &str, hits: i64) -> Topic {
    Topic {
        id,
        name: name.to_string(),
        keywords: format!("{name}, story"),
        active: id != 3,
        hits,
        last_hit: stamp(2),
    }
}

fn article(id: i64) -> Article {
    let (title, source, _, _, _, _) = story(id);
    Article {
        id,
        title: title.to_string(),
        source: if source == 1 { "The Nordic Post" } else { "Alpine Review" }.to_string(),
        published: stamp(id),
        summary: format!("{title} — a summary sentence for the list."),
        content: format!("{title}. The body of the story."),
        country: "DEU".to_string(),
        url: format!("https://example.invalid/{id}"),
        full_text: true,
    }
}

/// An open topic draws its stories as rows of its own, newest first, under its terms.
#[test]
fn an_open_topic_draws_its_stories_newest_first() {
    let ctx = context();
    // The archive would have read these: story 2 is older than story 1, and the page may
    // not reorder them, because the query's order is the archive's answer.
    let rows = vec![article(1), article(2)];
    let topics = [topic(1, "Chip supply", 3), topic(2, "Harbour labour", 2)];
    topics::remember_open(&ctx, Some(1));
    let painted = frame(&ctx, input(Vec::new(), 0.0), &topics, Stories::of(1, &rows));

    let newest = painted
        .at("Chip export rules")
        .unwrap_or_else(|| panic!("the newest story is drawn: {:?}", painted.text));
    let older = painted
        .at("A second foundry")
        .unwrap_or_else(|| panic!("the second story is drawn: {:?}", painted.text));
    assert!(newest < older, "newest first: {newest} then {older}");
    assert!(painted.any("The Nordic Post"), "the outlet is on the row");
    assert_eq!(painted.asked, None, "drawing a list asks for nothing");

    // The list is under the row it belongs to, and the row is still open on its terms.
    let terms = painted
        .at("Terms: ")
        .unwrap_or_else(|| panic!("the open topic's terms are drawn: {:?}", painted.text));
    assert!(terms < newest, "stories come after the terms: {terms} then {newest}");
}

/// A topic with no stories says so in a sentence — and says which of the two it is,
/// because "nothing ever matched" and "nothing inside the window" call for different
/// actions.
#[test]
fn a_topic_without_stories_says_so_in_a_sentence() {
    let ctx = context();
    let topics = [topic(3, "Alpine freight", 0)];
    topics::remember_open(&ctx, Some(3));

    // Never matched anything: `hits` is zero, and the shell read an empty list.
    let never = frame(&ctx, input(Vec::new(), 0.0), &topics, Stories::of(3, &[]));
    assert!(
        never.any("Nothing has matched this topic yet."),
        "the sentence: {:?}",
        never.text
    );
    assert_eq!(never.asked, None);

    // Matched before, nothing inside the window.
    let ctx = context();
    let topics = [topic(1, "Chip supply", 128)];
    topics::remember_open(&ctx, Some(1));
    let outside = frame(&ctx, input(Vec::new(), 0.0), &topics, Stories::of(1, &[]));
    assert!(
        outside.any("This topic has no story inside the window."),
        "the sentence: {:?}",
        outside.text
    );
}

/// A shell that read nothing draws no story band at all — and a band read for *another*
/// topic is not drawn under this one. Claiming "no stories" without having looked would be
/// the page inventing an answer, and attributing one topic's stories to another would be
/// worse than drawing none.
#[test]
fn stories_read_for_another_topic_are_not_drawn() {
    let ctx = context();
    let topics = [topic(1, "Chip supply", 3), topic(2, "Harbour labour", 2)];
    topics::remember_open(&ctx, Some(2));

    // Nothing read at all.
    let nothing = frame(&ctx, input(Vec::new(), 0.0), &topics, Stories::none());
    assert!(!nothing.any("Nothing has matched this topic yet."), "no claim: {:?}", nothing.text);
    assert!(!nothing.any("This topic has no story inside the window."), "{:?}", nothing.text);
    assert!(nothing.at("Chip export rules").is_none());

    // Topic 1's rows in hand while topic 2 is open.
    let ctx = context();
    topics::remember_open(&ctx, Some(2));
    let rows = vec![article(1)];
    let mismatched = frame(&ctx, input(Vec::new(), 0.0), &topics, Stories::of(1, &rows));
    assert!(
        mismatched.at("Chip export rules").is_none(),
        "another topic's story is not drawn here: {:?}",
        mismatched.text
    );
    assert_eq!(mismatched.asked, None);
}

/// A click on a story row answers that story's id: the row is the thing that was clicked,
/// so the reading pane opens the story the reader pointed at and not the newest one.
#[test]
fn a_click_on_a_story_row_answers_its_id() {
    let ctx = context();
    let topics = [topic(1, "Chip supply", 3)];
    topics::remember_open(&ctx, Some(1));
    let rows = vec![article(1), article(2)];

    // Frame one lays the page out and says where the second story landed.
    let first = frame(&ctx, input(Vec::new(), 0.0), &topics, Stories::of(1, &rows));
    let older = first
        .rect_of("A second foundry raises its capacity")
        .unwrap_or_else(|| panic!("the second story is drawn: {:?}", first.text));

    // `egui` reports a click on the release, at the point the press happened, so the
    // pointer moves first — the widget under the point is the one that gets the press.
    let at = older.center();
    let press = |pressed: bool| {
        input(
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            if pressed { 1.0 / 60.0 } else { 2.0 / 60.0 },
        )
    };
    frame(&ctx, press(true), &topics, Stories::of(1, &rows));
    let released = frame(&ctx, press(false), &topics, Stories::of(1, &rows));
    assert_eq!(
        released.asked,
        Some(2),
        "the clicked story, not the newest one: {:?}",
        released.text
    );
}

/// Enter on the row the keyboard is on answers that story too: the table's keys belong to
/// the whole table, and a reader who never touches the mouse can open a topic's story.
#[test]
fn enter_on_a_story_row_answers_its_id() {
    let ctx = context();
    let topics = [topic(1, "Chip supply", 3)];
    topics::remember_open(&ctx, Some(1));
    let rows = vec![article(1), article(2)];

    // Put the keyboard on the older story, the way the arrow keys do: by id, so the test
    // does not need the row to have been painted first.
    ctx.memory_mut(|memory| memory.request_focus(topics::story_row_id(1, 2)));
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let painted = frame(&ctx, input(vec![enter], 0.0), &topics, Stories::of(1, &rows));
    assert_eq!(
        painted.asked,
        Some(2),
        "Enter on the focused story: {:?}",
        painted.text
    );
}

/// The same Enter on a *topic* row still opens the topic rather than a story: the two
/// keyboards are separate rows, and the story band only answers for its own rows.
#[test]
fn enter_on_a_topic_row_opens_the_topic_and_asks_for_no_story() {
    let ctx = context();
    let topics = [topic(1, "Chip supply", 3), topic(2, "Harbour labour", 2)];
    let rows = vec![article(1)];

    ctx.memory_mut(|memory| {
        memory.request_focus(atlas_desktop::views::topics::table::row_id("topics", 1))
    });
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let painted = frame(&ctx, input(vec![enter], 0.0), &topics, Stories::of(1, &rows));
    assert_eq!(painted.asked, None, "opening a topic is not opening a story");
    assert_eq!(
        topics::open_topic(&ctx),
        Some(1),
        "and the row the keyboard was on is the one that opened"
    );
    // The story band followed the row open without a second frame.
    assert!(painted.at("Chip export rules").is_some(), "{:?}", painted.text);
}

// ---------------------------------------------------------------------------
// the layout: the list fills the page it was given
// ---------------------------------------------------------------------------

/// Enough topics to overflow a tall window, so "it scrolls" is measured and not assumed.
fn many_topics(count: i64) -> Vec<Topic> {
    (1..=count)
        .map(|id| Topic {
            id,
            name: format!("Chip supply {id}"),
            keywords: format!("chip, wafer, export {id}"),
            active: id % 3 != 0,
            hits: id * 7,
            last_hit: stamp(id),
        })
        .collect()
}

/// The rows region reaches the bottom of the page box at the smallest window, a common one
/// and the largest audited one.
///
/// Measured before the fix (`table::rows_height` reading `viewport_rect().bottom()`): the
/// block of rows was content-high at every size — 112 px in a 450 px box at 700×520, and
/// 112 px in a 1330 px box at 2400×1400 — because the scroll area shrank to its content
/// and no height was ever asked of it. The list is the page; it has to take the box.
#[test]
#[test]
fn the_rows_region_fills_the_page_box_at_every_size() {
    for size in [
        Vec2::new(700.0, 520.0),
        Vec2::new(1280.0, 820.0),
        Vec2::new(2400.0, 1400.0),
    ] {
        // Two lists, because "fills the box" may not depend on the content: three topics is
        // the archive's own shape (it holds three) and the case the audit measured; forty is
        // a list longer than any window, which has to scroll inside the same region rather
        // than make the page taller.
        for count in [3_i64, 40] {
            let (painted, page_box, region) = filled(size, count);
            eprintln!(
                "{:>9} window, {count:>2} topics: page box {:>6.1}×{:>6.1} at y {:.1}…{:.1} · \
                 rows region {:.1}×{:.1} at y {:.1}…{:.1} · painted text bbox {:.1}×{:.1}",
                format!("{}×{}", size.x as i64, size.y as i64),
                page_box.width(),
                page_box.height(),
                page_box.top(),
                page_box.bottom(),
                region.width(),
                region.height(),
                region.top(),
                region.bottom(),
                painted.bbox().width(),
                painted.bbox().height(),
            );

            let gap = page_box.bottom() - region.bottom();
            assert!(
                gap.abs() <= 4.0,
                "{size:?}, {count} topics: the rows stop {gap:.1} px above the page box bottom \
                 (page box {page_box:?}, region {region:?})"
            );
            // The width: the box, less the gutter the scroll bar paints in, and nothing else.
            assert!(
                (page_box.width() - region.width()).abs() <= 16.0,
                "{size:?}: the rows are {:.1} px wide in a {:.1} px box",
                region.width(),
                page_box.width()
            );
            assert!(
                region.top() > page_box.top() + 40.0,
                "{size:?}: the rows start on top of the header: {region:?} in {page_box:?}"
            );
            // And the region is the page's own: a list that filled the *viewport* would run
            // under the status row.
            assert!(
                region.bottom() <= page_box.bottom(),
                "{size:?}: the rows run past the page box: {region:?} vs {page_box:?}"
            );
            // The first row is drawn, so an empty region is not a pass.
            assert!(
                painted.at("Chip supply 1").is_some(),
                "{size:?}: the list painted no row: {:?}",
                painted.text
            );
        }
    }
}

/// One window of `size` showing `count` topics: the last frame's paint, the box the page was
/// given, and the rows region it claimed inside it.
fn filled(size: Vec2, count: i64) -> (Painted, Rect, Rect) {
    let ctx = context();
    let topics = many_topics(count);
    // Two frames: the first lays the panels out, the second draws in the box the first gave
    // them (the same reason `tests/smoke.rs` draws three).
    let mut page_box = Rect::NOTHING;
    let mut painted = Painted::default();
    for index in 0..2 {
        let (drawn, box_) = frame_in(
            &ctx,
            input_at(size, Vec::new(), index as f64 / 60.0),
            &topics,
            Stories::none(),
        );
        page_box = box_;
        painted = drawn;
    }
    let region = topics::rows_region(&ctx)
        .unwrap_or_else(|| panic!("{size:?}: the page published no rows region"));
    (painted, page_box, region)
}

/// The column header stays at the top of the region while the rows scroll under it, and no
/// row is ever drawn outside the region.
///
/// This is the other half of "the list fills the window": a page that filled the box by
/// wrapping the whole table in one scroll area would scroll its header away, and a page
/// that painted rows with a raw painter would spill them into the margins. Both are
/// measured here, not assumed.
#[test]
fn the_header_stays_at_the_top_while_the_rows_scroll() {
    let ctx = context();
    let size = Vec2::new(1280.0, 820.0);
    let topics = many_topics(40);

    // Frame one: where the header and the first row are.
    let (before, page_box) = frame_in(&ctx, input_at(size, Vec::new(), 0.0), &topics, Stories::none());
    let region = topics::rows_region(&ctx).expect("the page published its rows region");
    let header = before
        .rect_of("name")
        .unwrap_or_else(|| panic!("the header is painted: {:?}", before.text));
    let first = before
        .rect_of("Chip supply 1")
        .unwrap_or_else(|| panic!("the first row is painted: {:?}", before.text));
    eprintln!(
        "1280×820: page box {page_box:?} · rows region {region:?} · header y {:.1} · first row y {:.1}",
        header.top(),
        first.top()
    );

    // A wheel over the list: the pointer moves onto it, then the wheel turns.
    let at = region.center();
    let wheel = |time: f64| {
        input_at(
            size,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -240.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            time,
        )
    };
    // The wheel, then the frames its scroll animation settles over.
    let mut after = Painted::default();
    let mut settled_box = Rect::NOTHING;
    for step in 1..=8 {
        let events = if step == 1 { wheel(step as f64 / 60.0).events } else { Vec::new() };
        let (drawn, box_) =
            frame_in(&ctx, input_at(size, events, step as f64 / 60.0), &topics, Stories::none());
        after = drawn;
        settled_box = box_;
    }

    let header_after = after
        .rect_of("name")
        .unwrap_or_else(|| panic!("the header is painted after scrolling: {:?}", after.text));
    assert_eq!(
        (header.top(), header.bottom()),
        (header_after.top(), header_after.bottom()),
        "the header moved while the rows scrolled"
    );

    // The list really moved: the row that was first is now higher, or gone out of the
    // region, and the rows that fill the top of the region are later ones.
    match after.visible_exact("Chip supply 1") {
        None => {}
        Some(rect) => assert!(
            rect.top() < first.top(),
            "the list did not scroll: the first row is at {:.1}, was {:.1}",
            rect.top(),
            first.top()
        ),
    }
    let top_row = after
        .visible("Chip supply ")
        .into_iter()
        .map(|(text, rect)| (text, rect))
        .min_by(|left, right| left.1.top().total_cmp(&right.1.top()));
    let top_row = top_row.unwrap_or_else(|| panic!("no row at all after scrolling"));
    assert_ne!(
        top_row.0, "Chip supply 1",
        "the first topic is still the topmost row: nothing scrolled"
    );
    eprintln!(
        "after scrolling: header y {:.1} (unchanged) · topmost row {:?} at y {:.1}",
        header_after.top(),
        top_row.0,
        top_row.1.top()
    );

    // No row outside the region: every row that is visible sits inside it, and the region
    // is still the box the page was given (its top is where the header ends; its bottom is
    // the page box's, less the one unit of air).
    let region_after = topics::rows_region(&ctx).expect("the region was published again");
    assert!(
        (region_after.top() - region.top()).abs() < 0.5,
        "the rows region's top moved while the list scrolled: {region:?} then {region_after:?}"
    );
    let gap = settled_box.bottom() - region_after.bottom();
    assert!(
        gap.abs() <= 4.0,
        "the region no longer reaches the page box: gap {gap:.1} \
         (box {settled_box:?}, region {region_after:?})"
    );
    // A row that is wholly inside the region is painted there — the list is in the box,
    // not under it — and at least one row is wholly inside after the scroll.
    let slack = region_after.expand(1.0);
    let whole: Vec<String> = after
        .visible("Chip supply ")
        .into_iter()
        .filter(|(_, rect)| slack.contains_rect(*rect))
        .map(|(text, _)| text)
        .collect();
    assert!(
        !whole.is_empty(),
        "no row is wholly inside the region {region_after:?}: the list is not in it"
    );
    // And every row is drawn under a scissor that is the region's own vertical extent, so
    // a row that runs past the region's edge (the partly scrolled one at the bottom, or the
    // ones scrolled above) is cut there and not painted over the header or the page. A page
    // that drew its rows with a raw painter, or under the panel's scissor instead of the
    // scroll area's, fails this line. Only the vertical extent is the scroll area's:
    // `egui` narrows the scissor of the axis that scrolls (`scroll_area.rs:897-909`), and
    // this list scrolls one way.
    for (text, _rect, clip) in &after.clipped {
        if text.starts_with("Chip supply ") {
            assert!(
                clip.top() >= region_after.top() - 1.0 && clip.bottom() <= region_after.bottom() + 1.0,
                "{text:?} is drawn with a scissor {clip:?} that leaves the rows region \
                 {region_after:?}"
            );
        }
    }
}
