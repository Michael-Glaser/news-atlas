//! The Monitors page's story list: what the archive answers, and what the page does with it.
//!
//! The complaint this file pins down is the one the Topics page had: a reader opens a
//! monitor, reads its question, and the stories that monitor collected are nowhere. Two
//! halves are tested here, and they are tested apart because they fail apart:
//!
//! * **The read** (`data::Archive::monitor_stories`). A temporary archive, opened through
//!   the same `SQLITE_OPEN_READ_ONLY` connection the window uses, is asked for one
//!   monitor's stories. What must hold: newest first, only that monitor's, only inside the
//!   window, only stories the Articles page would also list (`relevant`, not `dupe`),
//!   capped by the limit — and an unknown monitor answers *nothing* rather than failing,
//!   because a page can print "nothing collected" and cannot print an error.
//! * **The page** (`views::monitors::show_with`). Drawn with no window: the stories appear
//!   under the open monitor, newest first; a click on a story row returns that story's id;
//!   Enter on the focused row returns it too; a monitor that collected nothing says so in
//!   a sentence; and a name the archive filled with control characters, a NUL and five
//!   thousand capitals never breaks the layout.
//!
//! Nothing here reads `data/atlas.db`: the live archive belongs to the user and every
//! count would move under the test. Each fixture is a temp directory, removed on drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::data::{Archive, Article, Monitor};
use atlas_desktop::theme;
use atlas_desktop::views::monitors::{self, MonitorStories};
use atlas_desktop::views::topics::table;

/// The screen every frame is told about: the app's own default window size, so the page
/// lays out as it does on a real 1440×900 screen (`src/main.rs`).
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

/// The window the page is read in — the window `app.rs::reload` uses.
const HOURS: i64 = 72;

/// How many stories the shell asks for: the map's `PINS` limit, the widest list a page
/// shows at once.
const LIMIT: i64 = 400;

/// The monitor every fixture belongs to.
const MONITOR: i64 = 7;

/// The other monitor, whose stories must never appear under this one.
const OTHER: i64 = 8;

// ---------------------------------------------------------------------------
// a temporary archive
// ---------------------------------------------------------------------------

/// A `data/atlas.db` of our own: the tables `monitor_stories` reads, and nothing else.
///
/// `articles` and `sources` are the list's own columns; `monitor_seen` is what the monitor
/// has already judged (`key = "a<id>"`, the tool protocol's spelling) and `alerts` is what
/// it surfaced, because the query unions the two — a story can be in either alone.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

/// Keeps two archives built in the same nanosecond on different test threads apart.
static NEXT_ARCHIVE: AtomicU64 = AtomicU64::new(0);

impl TempArchive {
    /// An archive with a monitor's stories in it.
    ///
    /// Seeded so every rule the query claims is visible in one table:
    ///
    /// | id  | published | relevant | dupe | linked by     | in monitor 7's list? |
    /// |-----|-----------|----------|------|---------------|----------------------|
    /// | 101 | 2 h ago   | 1        | 0    | `monitor_seen`| yes, first           |
    /// | 102 | 5 h ago   | 1        | 0    | `alerts`      | yes, second          |
    /// | 103 | 26 h ago  | 1        | 0    | `monitor_seen`| yes, third           |
    /// | 104 | 1 h ago   | 1        | 0    | nothing       | no — never linked    |
    /// | 105 | 200 h ago | 1        | 0    | `monitor_seen`| no — outside 72 h    |
    /// | 106 | 3 h ago   | 0        | 0    | `monitor_seen`| no — irrelevant      |
    /// | 107 | 4 h ago   | 1        | 1    | `monitor_seen`| no — a duplicate     |
    /// | 201 | 1 h ago   | 1        | 0    | `monitor_seen`| no — the other one   |
    ///
    /// The first three are what the fixture's markers are for: the list is expected to be
    /// `[101, 102, 103]` in that order, and nothing else.
    fn build() -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-desktop-monitors-{}-{}-{}",
            std::process::id(),
            nanos,
            NEXT_ARCHIVE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("atlas.db");

        let conn = rusqlite::Connection::open(&file).expect("a fresh database");
        conn.execute_batch(
            "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE articles(
                 id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                 published TEXT, summary TEXT, content TEXT, place TEXT, iso TEXT,
                 url TEXT, full_text INTEGER, relevant INTEGER, dupe INTEGER,
                 lat REAL, lon REAL);
             CREATE TABLE monitor_seen(monitor_id INTEGER, key TEXT);
             CREATE TABLE alerts(id INTEGER PRIMARY KEY, monitor_id INTEGER, article_id INTEGER);
             CREATE TABLE monitors(
                 id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER,
                 cadence_minutes INTEGER, last_run TEXT, hits INTEGER);",
        )
        .expect("the schema monitor_stories reads");

        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1, 'The Nordic Post'), (2, 'Alpine Review')",
            [],
        )
        .expect("two sources");
        conn.execute(
            "INSERT INTO monitors(id, name, query, active, cadence_minutes, last_run, hits) \
             VALUES(?1, 'Harbour labour', 'harbour strike or dockworkers ballot', 1, 120, ?2, 3), \
                   (?3, 'Chip export rules', 'chip export restrictions', 0, 0, NULL, 0)",
            rusqlite::params![MONITOR, stamp(1), OTHER],
        )
        .expect("two monitors");

        // The stories, with the six columns the list reads.
        let stories: [(i64, &str, i64, i64, i64, i64); 8] = [
            (101, "Harbour crane strike ends after nine days", 1, 2, 1, 0),
            (102, "A second harbour approves the contract", 2, 5, 1, 0),
            (103, "Dockworkers ballot on the new shift", 1, 26, 1, 0),
            (104, "Alpine rail freight reaches a record", 2, 1, 1, 0),
            (105, "A story the window has left behind", 1, 200, 1, 0),
            (106, "A story the model called irrelevant", 2, 3, 0, 0),
            (107, "A story that was already told", 1, 4, 1, 1),
            (201, "Chip export rules tighten in Asia", 2, 1, 1, 0),
        ];
        for (id, title, source, hours_ago, relevant, dupe) in stories {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                   content, iso, url, full_text, relevant, dupe) \
                 VALUES(?1, ?2, '', ?3, ?4, '', '', 'DEU', '', 0, ?5, ?6)",
                rusqlite::params![id, title, source, stamp(hours_ago), relevant, dupe],
            )
            .expect("a story");
        }

        // 101, 103, 105, 106, 107 and the other monitor's 201 are *marks*: the monitor
        // judged them. 102 and 201 are *alerts*: the monitor surfaced them. One of each
        // spelling has to answer, because the query unions the two.
        conn.execute(
            "INSERT INTO monitor_seen(monitor_id, key) VALUES \
             (?1, 'a101'), (?1, 'a103'), (?1, 'a105'), (?1, 'a106'), (?1, 'a107'), (?2, 'a201'), \
             (?1, 'p42')",
            rusqlite::params![MONITOR, OTHER],
        )
        .expect("the marks");
        conn.execute(
            "INSERT INTO alerts(id, monitor_id, article_id) VALUES(1, ?1, 102), (2, ?2, 201)",
            rusqlite::params![MONITOR, OTHER],
        )
        .expect("two alerts");

        TempArchive { dir, file }
    }

    fn open(&self) -> Archive {
        Archive::open(&self.file).expect("the window opens the copy read-only")
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `hours_ago` as the archive's own stamp format (`YYYY-MM-DDTHH:MM:SSZ`), in UTC.
///
/// The archive's window predicate is a string compare against the same format, so a
/// fixture has to spell its stamps exactly as the collector does.
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

// ---------------------------------------------------------------------------
// the read
// ---------------------------------------------------------------------------

/// One monitor's stories: newest first, its own only, inside the window, listable.
#[test]
fn the_archive_answers_one_monitors_stories_newest_first() {
    let temp = TempArchive::build();
    let archive = temp.open();

    let ids: Vec<i64> = archive
        .monitor_stories(MONITOR, HOURS, LIMIT)
        .iter()
        .map(|story| story.id)
        .collect();
    assert_eq!(
        ids,
        vec![101, 102, 103],
        "the marks and the alerts, newest first, and nothing else of the eight seeded rows"
    );

    // The two spellings answer: 101 is a mark and 102 is an alert. A query that read only
    // one of the two tables would drop one of them.
    let stories = archive.monitor_stories(MONITOR, HOURS, LIMIT);
    assert!(stories.iter().any(|story| story.id == 101), "the mark is missing");
    assert!(stories.iter().any(|story| story.id == 102), "the alert is missing");

    // The stamp order is the row order: `published DESC` is what "newest first" means, and
    // a page that re-sorted by id would pass a list of ids and fail this.
    let stamps: Vec<&str> = stories.iter().map(|story| story.published.as_str()).collect();
    let mut sorted = stamps.clone();
    sorted.sort_unstable();
    sorted.reverse();
    assert_eq!(stamps, sorted, "the list is not in newest-first order");

    // A title comes back as the archive stored it, so a row can print something.
    assert_eq!(stories[0].title, "Harbour crane strike ends after nine days");

    // The limit is the caller's: asking for one answers the newest one.
    let first = archive.monitor_stories(MONITOR, HOURS, 1);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, 101, "the limit takes from the top of the order");
}

/// An unknown monitor, a window that holds nothing, and a limit of zero all answer an
/// empty list: a page prints "nothing collected" and has no way to print an error.
#[test]
fn an_unknown_monitor_and_an_empty_window_answer_nothing() {
    let temp = TempArchive::build();
    let archive = temp.open();

    assert!(archive.monitor_stories(999, HOURS, LIMIT).is_empty(), "an unknown monitor");
    assert!(archive.monitor_stories(0, HOURS, LIMIT).is_empty(), "no monitor at all");
    assert!(archive.monitor_stories(-3, HOURS, LIMIT).is_empty(), "a negative id");
    assert!(archive.monitor_stories(MONITOR, HOURS, 0).is_empty(), "a limit of zero");
    // A window narrower than the newest story's age: the monitor has stories, none here.
    assert!(
        archive.monitor_stories(MONITOR, 1, LIMIT).is_empty(),
        "a one-hour window holds none of the three"
    );
}

// ---------------------------------------------------------------------------
// the page, with no window
// ---------------------------------------------------------------------------

/// What one frame painted: every string, with the rectangle it landed in.
#[derive(Default)]
struct Painted {
    text: Vec<(String, Rect)>,
    /// The story the page asked the shell to open this frame.
    asked: Option<i64>,
}

impl Painted {
    /// The first string that starts with `start`, and where it landed.
    fn rect_of(&self, start: &str) -> Option<Rect> {
        self.text
            .iter()
            .find(|(text, _)| text.starts_with(start))
            .map(|(_, rect)| *rect)
    }

    /// Where the string that starts with `start` sits, top to bottom.
    fn top_of(&self, start: &str) -> Option<f32> {
        self.rect_of(start).map(|rect| rect.top())
    }

    fn any(&self, value: &str) -> bool {
        self.text.iter().any(|(text, _)| text == value)
    }

    fn contains(&self, part: &str) -> bool {
        self.text.iter().any(|(text, _)| text.contains(part))
    }

    fn rendered(&self) -> Vec<&str> {
        self.text.iter().map(|(text, _)| text.as_str()).collect()
    }
}

/// A context with the app's own style installed, as `Atlas::new` does.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

fn input(events: Vec<egui::Event>, time: f64) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(time),
        events,
        ..Default::default()
    }
}

/// One frame of the page, drawn the way `app.rs` draws it — a panel with the page's own
/// margin, no scrolling wrapper of the test's, because the page scrolls its own rows.
///
/// The frame's `TexturesDelta` is released before the `FullOutput` is dropped: `epaint
/// 0.37` panics in `Drop` when deltas are still unapplied, and there is no renderer here
/// to upload them. `shapes` — all this file asserts on — need no upload.
fn frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    monitors: &[Monitor],
    stories: MonitorStories<'_>,
) -> Painted {
    let mut painted = Painted::default();
    let output = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::PAGE)
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                painted.asked = monitors::show_with(ui, monitors, stories);
            });
    });
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut painted);
    }
    output.drop_without_applying_deltas();
    painted
}

fn walk(shape: &Shape, painted: &mut Painted) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, painted);
            }
        }
        Shape::Text(text) => painted
            .text
            .push((text.galley.text().to_string(), text.galley.rect.translate(text.pos.to_vec2()))),
        _ => {}
    }
}

/// The fixture's rows: a monitor as `Archive::monitors` would hand it over.
fn monitor(id: i64, name: &str, active: bool, hits: i64) -> Monitor {
    Monitor {
        id,
        name: name.to_string(),
        query: "harbour strike or dockworkers ballot".to_string(),
        active,
        cadence: 120,
        last_run: stamp(1),
        hits,
    }
}

// ---------------------------------------------------------------------------
// opening a row
// ---------------------------------------------------------------------------

/// Opening a monitor lists the stories it collected, newest first, under its question.
#[test]
fn opening_a_row_lists_its_stories_newest_first() {
    let temp = TempArchive::build();
    let archive = temp.open();
    let rows = archive.monitor_stories(MONITOR, HOURS, LIMIT);
    let monitors = archive.monitors();
    assert!(!monitors.is_empty(), "the fixture has a monitor row");

    let ctx = context();
    let shut = frame(&ctx, input(Vec::new(), 0.0), &monitors, MonitorStories::of(MONITOR, &rows));
    assert!(
        shut.rect_of("Harbour crane strike ends").is_none(),
        "a shut row drew a story: {:?}",
        shut.rendered()
    );

    monitors::remember_open(&ctx, Some(MONITOR));
    let open = frame(&ctx, input(Vec::new(), 1.0 / 60.0), &monitors, MonitorStories::of(MONITOR, &rows));

    // The three markers, in the order the archive answered them.
    let order = [
        "Harbour crane strike ends after nine days",
        "A second harbour approves the contract",
        "Dockworkers ballot on the new shift",
    ];
    let mut last = f32::NEG_INFINITY;
    for title in order {
        let top = open
            .top_of(title)
            .unwrap_or_else(|| panic!("{title:?} is not under the open row: {:?}", open.rendered()));
        assert!(top > last, "{title:?} is not below the story before it");
        last = top;
    }
    // …and nothing that is not one of them: the other monitor's story, the one outside the
    // window, the irrelevant one and the duplicate are all absent.
    for absent in [
        "Chip export rules tighten in Asia",
        "A story the window has left behind",
        "A story the model called irrelevant",
        "A story that was already told",
    ] {
        assert!(
            open.rect_of(absent).is_none(),
            "{absent:?} is not this monitor's story: {:?}",
            open.rendered()
        );
    }
    // The detail still says what the row could not: the question, the cadence, and the
    // monitor's state in a sentence.
    assert!(open.contains("Asks: harbour strike or dockworkers ballot"), "{:?}", open.rendered());
    assert!(open.contains("It is watching, has raised 3 alerts"), "{:?}", open.rendered());
}

/// A click on a story row returns that story's id — the row the reader pointed at, not the
/// newest one, so the reading pane opens what they chose.
#[test]
fn a_click_on_a_story_row_returns_its_id() {
    let temp = TempArchive::build();
    let archive = temp.open();
    let rows = archive.monitor_stories(MONITOR, HOURS, LIMIT);
    let monitors = archive.monitors();
    let ctx = context();
    monitors::remember_open(&ctx, Some(MONITOR));

    // Frame one lays the page out and says where the second story landed.
    let first = frame(&ctx, input(Vec::new(), 0.0), &monitors, MonitorStories::of(MONITOR, &rows));
    let at = first
        .rect_of("A second harbour approves the contract")
        .unwrap_or_else(|| panic!("the second story is drawn: {:?}", first.rendered()))
        .center();

    // `egui` reports a click on the release, at the point the press happened, so the
    // pointer moves first — the widget under the point is the one that gets the press.
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
    frame(&ctx, press(true), &monitors, MonitorStories::of(MONITOR, &rows));
    let released = frame(&ctx, press(false), &monitors, MonitorStories::of(MONITOR, &rows));
    assert_eq!(
        released.asked,
        Some(102),
        "the clicked story, not the newest one: {:?}",
        released.rendered()
    );
}

/// Enter on the row the keyboard is on returns that story too: a reader who never touches
/// the mouse can open what a monitor collected.
#[test]
fn enter_on_a_story_row_returns_it_too() {
    let temp = TempArchive::build();
    let archive = temp.open();
    let rows = archive.monitor_stories(MONITOR, HOURS, LIMIT);
    let monitors = archive.monitors();
    let ctx = context();
    monitors::remember_open(&ctx, Some(MONITOR));

    // The keyboard goes to the third story's row, by id, the way the arrow keys put it
    // there — no frame has to have painted it first.
    ctx.memory_mut(|memory| memory.request_focus(monitors::story_row_id(MONITOR, 103)));
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let painted = frame(&ctx, input(vec![enter], 0.0), &monitors, MonitorStories::of(MONITOR, &rows));
    assert_eq!(
        painted.asked,
        Some(103),
        "Enter on the focused story: {:?}",
        painted.rendered()
    );
}

/// Enter on a *monitor* row still opens the monitor and asks for no story: the two
/// keyboards are separate rows, and the story band only answers for its own.
#[test]
fn enter_on_a_monitor_row_asks_for_no_story() {
    let temp = TempArchive::build();
    let archive = temp.open();
    let rows = archive.monitor_stories(MONITOR, HOURS, LIMIT);
    let monitors = archive.monitors();
    let ctx = context();

    ctx.memory_mut(|memory| memory.request_focus(table::row_id("monitors", MONITOR)));
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let painted = frame(&ctx, input(vec![enter], 0.0), &monitors, MonitorStories::of(MONITOR, &rows));
    assert_eq!(painted.asked, None, "a monitor row is not a story: {:?}", painted.rendered());
    // …and it did open, which is what Enter on a row has always done.
    assert!(painted.contains("Asks: harbour strike"), "{:?}", painted.rendered());
}

// ---------------------------------------------------------------------------
// what is not there
// ---------------------------------------------------------------------------

/// A monitor that never collected anything says so; one that has alerts but no story in
/// the window says that instead. The two are different facts and the archive told us both.
#[test]
fn a_monitor_with_no_stories_says_the_sentence() {
    let ctx = context();
    monitors::remember_open(&ctx, Some(MONITOR));

    let empty = [monitor(MONITOR, "Harbour labour", true, 0)];
    let painted = frame(&ctx, input(Vec::new(), 0.0), &empty, MonitorStories::of(MONITOR, &[]));
    assert!(
        painted.any("This monitor has not collected anything yet."),
        "a monitor with no hits at all: {:?}",
        painted.rendered()
    );

    let had_hits = [monitor(MONITOR, "Harbour labour", true, 12)];
    let painted = frame(
        &ctx,
        input(Vec::new(), 1.0 / 60.0),
        &had_hits,
        MonitorStories::of(MONITOR, &[]),
    );
    assert!(
        painted.any("This monitor has no story inside the window."),
        "a monitor with alerts, none in the window: {:?}",
        painted.rendered()
    );
}

/// A shell that read nothing draws no story band at all — not an empty one. The page may
/// not claim a monitor collected nothing when it was never told.
#[test]
fn nothing_read_draws_no_story_band() {
    let ctx = context();
    monitors::remember_open(&ctx, Some(MONITOR));
    let monitors = [monitor(MONITOR, "Harbour labour", true, 3)];
    let painted = frame(&ctx, input(Vec::new(), 0.0), &monitors, MonitorStories::none());

    assert!(
        !painted.contains("collected anything yet") && !painted.contains("inside the window"),
        "the page claimed something about a list it was not given: {:?}",
        painted.rendered()
    );
    // The row still opened and still shows its own detail.
    assert!(painted.contains("Asks: harbour strike"), "{:?}", painted.rendered());
}

/// The stories in hand while a *different* row is open cannot be attributed to this one.
#[test]
fn another_monitors_stories_are_not_drawn_under_this_one() {
    let ctx = context();
    monitors::remember_open(&ctx, Some(OTHER));
    let monitors = [
        monitor(MONITOR, "Harbour labour", true, 3),
        monitor(OTHER, "Chip export rules", false, 0),
    ];
    let rows = vec![article(101, "Harbour crane strike ends after nine days")];
    let painted = frame(&ctx, input(Vec::new(), 0.0), &monitors, MonitorStories::of(MONITOR, &rows));
    assert!(
        painted.rect_of("Harbour crane strike ends").is_none(),
        "one monitor's story under another's row: {:?}",
        painted.rendered()
    );
    assert_eq!(painted.asked, None);
}

/// A paused monitor says it in the row *and* in its detail: "paused" between a dot and a
/// count is easy to read past, and it changes what the reader should expect.
#[test]
fn a_paused_monitor_says_paused_in_the_row_and_the_detail() {
    let ctx = context();
    monitors::remember_open(&ctx, Some(OTHER));
    let monitors = [monitor(OTHER, "Chip export rules", false, 0)];
    let painted = frame(&ctx, input(Vec::new(), 0.0), &monitors, MonitorStories::of(OTHER, &[]));

    assert!(painted.any("paused"), "the row's state column: {:?}", painted.rendered());
    assert!(
        painted.contains("It is paused: it asks its question again only when you ask it."),
        "the detail in words: {:?}",
        painted.rendered()
    );
}

/// An article fixture for the two tests that build rows by hand.
fn article(id: i64, title: &str) -> Article {
    Article {
        id,
        title: title.to_string(),
        source: "The Nordic Post".to_string(),
        published: stamp(2),
        summary: String::new(),
        content: String::new(),
        country: "DEU".to_string(),
        url: String::new(),
        full_text: false,
    }
}

// ---------------------------------------------------------------------------
// a hostile name
// ---------------------------------------------------------------------------

/// The longest a row's name may be drawn: `NAME_MAX` characters plus the ellipsis `clip`
/// appends. Written here as a rule rather than imported, because the point is that some
/// bound holds, not that this number is the bound.
const NAME_LIMIT: usize = 121;

/// A name the archive filled with everything a row cannot hold — five thousand capitals, a
/// newline, a NUL, a tab and a run of spaces — never breaks the layout: what is drawn is
/// one line, no control characters survive, the row keeps its height, nothing leaves the
/// page, and the same is true of a hundred-kilobyte query.
#[test]
fn a_hostile_name_never_breaks_the_layout() {
    let hostile = format!(
        "{}\n{}\0{}\t{}",
        "A".repeat(5_000),
        "B".repeat(40),
        "C".repeat(10),
        "D".repeat(10)
    );
    let mut bad = monitor(MONITOR, &hostile, true, 2_000);
    bad.query = "q".repeat(100_000);
    let monitors = [bad];
    let ctx = context();
    monitors::remember_open(&ctx, Some(MONITOR));
    let painted = frame(&ctx, input(Vec::new(), 0.0), &monitors, MonitorStories::of(MONITOR, &[]));

    for (text, rect) in &painted.text {
        // A newline is allowed in the *prose* — `asks` is two sentences on two lines by
        // design, and a wrapped label is the thing that can hold it. Every other control
        // character is a bug anywhere, and the row's cells are checked below for all of
        // them, newline included.
        assert!(
            !text.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control character reached the painter: {text:?}"
        );
        assert!(
            rect.min.x >= -1.0 && rect.max.x <= SCREEN.x + 1.0,
            "{text:?} left the page: {rect:?}"
        );
    }

    // The name is one line in a fixed-height row: `LayoutJob::simple_singleline` still
    // starts a second line on a newline, and a second line would paint over the next row.
    let name = painted
        .rect_of("AAAA")
        .unwrap_or_else(|| panic!("the name is drawn: {:?}", painted.rendered()));
    assert!(
        name.height() <= table::ROW_H,
        "the name's line is {} px tall, taller than the {} px row it sits in",
        name.height(),
        table::ROW_H
    );
    let drawn = painted
        .text
        .iter()
        .find(|(text, _)| text.starts_with("AAAA"))
        .map(|(text, _)| text)
        .expect("the name is drawn");
    assert!(
        !drawn.chars().any(char::is_control),
        "the row's name holds a control character: {drawn:?}"
    );
    assert!(
        drawn.chars().count() <= NAME_LIMIT,
        "the name was not bounded: {} characters",
        drawn.chars().count()
    );

    // The query is prose that wraps, so it is longer — and still bounded, and still one
    // part of the page rather than a hundred thousand characters laid out per frame.
    let asks = painted
        .text
        .iter()
        .find(|(text, _)| text.starts_with("Asks: "))
        .map(|(text, _)| text)
        .expect("the query is drawn");
    assert!(
        asks.chars().count() < 700,
        "the query was not bounded: {} characters",
        asks.chars().count()
    );
    assert!(asks.contains('q'), "the query is still what the archive stored: {asks:?}");
}
