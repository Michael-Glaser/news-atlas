//! "Wenn ich einen Filter auswähle, kommt einfach gar nichts."
//!
//! Two complaints in one sentence, and they had two different causes. Both were measured
//! on this fixture before anything changed, and every number in this file is a number a
//! frame painted.
//!
//! # What was wrong
//!
//! **(a) The Articles view never applied its own needle.** `views::articles::show` was
//! handed four stories and a view whose `needle` was `"chip"`, and it painted all four —
//! the assertion below is exactly that measurement, taken again with the fix reverted:
//! `left: ["Chip export rules tighten in Asia", "Harbour crane strike ends", "Timber
//! prices settle on the coast", "Ferry traffic resumes in the south"]`. The narrowing the
//! reader saw was only ever the *query* `crate::app::Atlas::reload` ran. That is a wrong
//! boundary: the view is the thing that knows what the search box says, and a view that
//! draws rows read for a different needle draws a list that does not mean what its
//! heading says.
//!
//! **(b) The app then put unrelated rows into that list.** `Atlas::reload` tops the list
//! up with the newest story at every map pin, so a dot on the map opens something. Those
//! rows are *not* filtered by the needle. Measured through the real window on this
//! fixture, a search for `chip` — one matching story — painted **three**: "Chip export
//! rules tighten in Asia", "Harbour crane strike ends", "Timber prices settle on the
//! coast". The list is capped at 400 rows and that top-up may read 600 more by id, so on
//! the shipped archive the search's own answer can be one row among many the reader did
//! not ask for, newest first — which is what "nothing comes" means from the reader's
//! side: nothing they searched for is where they are looking.
//!
//! # What fixes it, and where
//!
//! The narrowing moved into `views::articles::show` (the view filters the rows it draws
//! by its own needle, and counts the rows it lists), so the list can no longer disagree
//! with the box above it — not for one frame while a read is in flight, and not because
//! the caller appended pin stories. Two sentences follow from that: the heading says
//! "1 of 3 stories match" instead of counting rows that are not listed, and a story the
//! *search* hides gets its own sentence instead of "That story is no longer in the
//! window.", which was a lie about the archive.
//!
//! The remaining half is in `src/app.rs`, which this task does not own: the pin top-up
//! should not run while a needle is set (the map is not on screen, and a dot is a
//! request for one story, not for a search result). The view filter makes that
//! invisible to the reader; it still costs a read of up to 600 rows and it still makes
//! the *count* handed to the view wrong. Reported, not edited.
//!
//! # What is tested here
//!
//! Headless frames over `views::articles::show` on a fixture archive (1–4), the same
//! through the real window `Atlas::with_archive` + `Atlas::draw` (5–7), and the rail's
//! window filter re-reading and repainting rather than keeping the previous window's
//! rows (6–7). The last two tests are the layout the audit measured: one page in two
//! shapes, chosen by the page's own width, measured at 700×520, 900×620, 1280×820 and
//! 1680×1200. No port is opened, no window is shown: `egui::Context::run_ui` with a
//! 1440×900 screen (or the audit's four), the size `src/main.rs` opens.
//!
//! # The layout half
//!
//! The pane collapsed because the split was `default_size(520.0)` — a width written into
//! the view, which is a width that is wrong on some screen. It is now a share with a floor
//! and a ceiling, and below `SPLIT_FLOOR` the page stacks. Measured here, before → after:
//!
//! | window | page | list before | pane before | list after | pane after | shape after |
//! |---|---|---|---|---|---|---|
//! | 700×520 | 596 | 321 | **258** | 584 | 565 | stacked |
//! | 900×620 | 796 | 321 | 455 | 784 | 775 | stacked |
//! | 1280×820 | 1052 | 321 | 707 | 473 | 550 | split |
//! | 1680×1200 | 1452 | 321 | 1098 | 640 | 775 | split |
//!
//! (At the 560×480 and 620×600 windows the same "before" measured a pane of 107 and 179 px;
//! the audit's own run, before the rail learned to fold, measured 0 px at 700×520 — the
//! list took 471 of the 473 px page and no pane was painted at all.)
//!
//! # The row half
//!
//! A blind-click audit of the real window (156 clicks on a grid) found Articles the worst
//! page at 2.6 %: a row answered on its title line only, and the `10:53 · SOURCE` line and
//! the teaser under it were dead. The row is now one band — title, meta, summary and the
//! hairline that closes it — with one `Response` over all of it, the selection drawn behind
//! it and the focus ring around it. Measured through the real window, a 4 px sweep down a
//! row: **6 of 18 points answered before (33 %, dead from 24 px down), 17 of 17 after
//! (100 %)**. ↑/↓ move the ring (egui's own focus navigation) and the selection follows it,
//! so the keyboard and the mouse are one path; `Clear` is offered only when the box has
//! something in it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::data::{Archive, Article};
use atlas_desktop::theme;
use atlas_desktop::views::articles::{self, ArticlesView};

/// The window `src/main.rs` opens.
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

/// The window the app opens on (`src/app.rs`), and the one the rail's middle button asks
/// for. Two of the fixture's four stories are inside it.
const HOURS: i64 = 72;

/// The window the page-on-its-own tests draw: the rail's widest, so the page has all four
/// stories in hand and a needle has something to narrow.
const WIDE: i64 = 168;

// ---------------------------------------------------------------------------
// a fixture archive: four stories, four places, so the archive has pins
// ---------------------------------------------------------------------------

/// A `data/atlas.db` of this test's own, removed when the test ends.
///
/// The places are what make this fixture witness the reported bug: an archive with no
/// coordinates has no pins, and `Atlas::reload`'s top-up then appends nothing.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

/// Keeps two fixtures built on different threads from colliding when the clock's
/// resolution is coarser than a thread switch.
static NEXT_ARCHIVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl TempArchive {
    fn build() -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-search-{}-{}-{}",
            std::process::id(),
            nanos,
            NEXT_ARCHIVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
             CREATE TABLE alerts(id INTEGER PRIMARY KEY);",
        )
        .expect("the schema data.rs reads");
        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'Madrid Desk'),(2,'Elsewhere')",
            [],
        )
        .expect("two sources");

        // One story per place, so every story is also a pin's destination — the shape
        // that made the top-up append two rows to a search that matched one.
        //
        // `content` carries a marker per story: the reading pane paints `content`, the
        // list paints only titles and summaries, so a marker on screen means that story
        // is open in the pane and not merely listed.
        //
        // The body is a paragraph rather than one short line, and that is what makes a
        // layout assertion possible: a label wraps at the width the pane gives it, so the
        // body galley's rectangle *is* the reading column's width. A one-line body would
        // measure its own sentence and nothing about the page.
        let paragraph = "The desk kept the tape of the briefing and transcribed it line by \
             line, so the body of this story is long enough to wrap across the column and to \
             fill it, which is the only way a reader can tell a column from a margin.";
        //
        // Hours ago: 2 (chip), 20 (harbour), 50 (timber), 100 (ferry). Two stories are
        // inside 24 h, three inside 72 h, four inside 168 h — so the rail's three choices
        // are three different lists and each one is provable.
        let stories: [(i64, &str, &str, i64, &str, f64, f64); 4] = [
            (1, "Chip export rules tighten in Asia", "PANE-CHIP", 2, "Tokyo", 35.7, 139.7),
            (2, "Harbour crane strike ends", "PANE-HARBOUR", 20, "Hamburg", 54.0, 10.0),
            (3, "Timber prices settle on the coast", "PANE-TIMBER", 50, "Vancouver", 49.3, -123.1),
            (4, "Ferry traffic resumes in the south", "PANE-FERRY", 100, "Cape Town", -33.9, 18.4),
        ];
        for (id, title, marker, hours_ago, place, lat, lon) in stories {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', 1, ?3, ?4, ?5, 'ZZ', ?6, ?7, 1, 1, 0, ?8, ?9)",
                rusqlite::params![
                    id,
                    title,
                    stamp(hours_ago),
                    format!("{title} — a summary sentence."),
                    format!("{marker}: the body of this story. {paragraph}"),
                    place,
                    format!("https://example.invalid/{id}"),
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        drop(conn);
        TempArchive { dir, file }
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`).
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

const TITLES: [&str; 4] = [
    "Chip export rules tighten in Asia",
    "Harbour crane strike ends",
    "Timber prices settle on the coast",
    "Ferry traffic resumes in the south",
];

const MARKERS: [&str; 4] = ["PANE-CHIP", "PANE-HARBOUR", "PANE-TIMBER", "PANE-FERRY"];

// ---------------------------------------------------------------------------
// one headless frame, and what it painted
// ---------------------------------------------------------------------------

/// The strings one frame painted, each with the rectangle its galley occupies — the
/// rectangle is how a test clicks a button it can only see as words.
///
/// `runs` keeps the scissor each string was drawn under, and `fills` the filled
/// rectangles (a selected row, the page's own panels): the layout assertions are about
/// geometry the reader can see, so they are read off the frame rather than recomputed
/// from the view's constants — a test that repeated the arithmetic would agree with a
/// wrong layout.
#[derive(Default)]
struct Frame {
    text: String,
    texts: Vec<(Rect, String)>,
    runs: Vec<(Rect, Rect, String)>,
    fills: Vec<Rect>,
    /// Stroked rectangles: the focus ring is one, and the ring is the thing a keyboard
    /// reader sees, so it has to be readable out of a frame like the text is.
    outlines: Vec<(Rect, f32)>,
}

impl Frame {
    fn paints(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    /// The stories of this fixture that the frame painted as list rows or as a pane title.
    fn stories(&self) -> Vec<&'static str> {
        TITLES
            .iter()
            .copied()
            .filter(|title| self.paints(title))
            .collect()
    }

    fn markers(&self) -> Vec<&'static str> {
        MARKERS
            .iter()
            .copied()
            .filter(|marker| self.paints(marker))
            .collect()
    }

    /// Where the string `label` was painted, exactly once, or a failure that shows what
    /// the frame did paint instead.
    fn at(&self, label: &str) -> Pos2 {
        let found: Vec<Rect> = self
            .texts
            .iter()
            .filter(|(_, text)| text == label)
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "the frame painted {label:?} {} times; it painted {:?}",
            found.len(),
            self.texts.iter().map(|(_, text)| text).collect::<Vec<_>>()
        );
        found[0].center()
    }

    fn labels(&self) -> Vec<&str> {
        self.texts.iter().map(|(_, text)| text.as_str()).collect()
    }
}

fn record(shapes: &[egui::epaint::ClippedShape]) -> Frame {
    let mut frame = Frame::default();
    for clipped in shapes {
        walk(&clipped.shape, clipped.clip_rect, &mut frame);
    }
    frame
}

fn walk(shape: &Shape, clip: Rect, frame: &mut Frame) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, clip, frame);
            }
        }
        Shape::Text(text) => {
            frame.text.push_str(text.galley.text());
            frame.text.push('\n');
            let rect = Rect::from_min_size(text.pos, text.galley.size());
            frame.texts.push((rect, text.galley.text().to_string()));
            frame.runs.push((rect, clip, text.galley.text().to_string()));
        }
        Shape::Rect(rect) if rect.fill.a() > 0 => frame.fills.push(rect.rect),
        Shape::Rect(rect) => frame.outlines.push((rect.rect, rect.stroke.width)),
        _ => {}
    }
}
fn input_at(events: Vec<egui::Event>, screen: Vec2) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
        events,
        ..Default::default()
    }
}

/// A context styled as the app styles it.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

// ---------------------------------------------------------------------------
// (1–4) the page on its own: `views::articles::show`
// ---------------------------------------------------------------------------

/// The page as `src/app.rs` would draw it, without the window around it: a click or a
/// keystroke is a real `egui::Event` at a real pixel, not a call into the view's fields.
struct ArticlesPage {
    ctx: egui::Context,
    view: ArticlesView,
    stories: Vec<Article>,
    hours: i64,
    /// The window this page is drawn in: the audit sizes are windows, and the page under
    /// test is what the rail and the page margins leave of one.
    screen: Vec2,
}

impl ArticlesPage {
    /// A page showing every story of `hours` — the rows a read with an *empty* box
    /// returned, which is what the reader's first keystroke lands on.
    fn new(archive: &Archive, hours: i64) -> ArticlesPage {
        let stories = archive.articles(hours, 400, "");
        assert_eq!(
            stories.len(),
            4,
            "the fixture must hold four stories in the window, or nothing here is provable"
        );
        ArticlesPage {
            ctx: context(),
            view: ArticlesView::default(),
            stories,
            hours,
            screen: SCREEN,
        }
    }

    /// One frame with these events, and what it painted.
    fn frame(&mut self, events: Vec<egui::Event>) -> Frame {
        let view = &mut self.view;
        let stories = &self.stories;
        let hours = self.hours;
        let screen = self.screen;
        let output = self.ctx.run_ui(input_at(events, screen), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                articles::show(ui, view, stories, hours);
            });
        });
        // A headless frame has no renderer to upload texture deltas to, and `epaint`
        // panics on a `FullOutput` dropped with deltas still in it.
        let painted = record(&output.shapes);
        output.drop_without_applying_deltas();
        painted
    }

    /// A frame with no input — the shape a repaint takes.
    fn look(&mut self) -> Frame {
        self.frame(Vec::new())
    }

    /// Press and release the primary button at `at`; return the frame that saw the
    /// release, which is the frame egui reports `clicked()` in.
    fn click(&mut self, at: Pos2) -> Frame {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)])
    }

    /// Click into the empty search box, so the next `Text` events land in it.
    fn focus_the_box(&mut self) {
        let at = {
            let painted = self.look();
            painted.at("a name, a place, a phrase")
        };
        self.click(at);
        let _ = self.look();
    }

    /// Type `text` one character per frame, the way a reader types, and return the frame
    /// the reader ends up looking at.
    ///
    /// One character per frame matters: it is the frame in which the keystroke is applied
    /// that a stale list would show through. The frame returned is the one *after* the
    /// last character, and that is the window's own behaviour rather than a test's
    /// convenience — the box's edit runs inside `show`, after the heading's count was laid
    /// out, so the keystroke lands on the list one frame later, and `egui` itself asks for
    /// that frame (`egui-0.37.0/src/widgets/text_edit/builder.rs:924`: "The galley is now
    /// stale" → `request_repaint`). Bounding the lag to one frame *is* the fix: the list is
    /// narrowed by the box, never waiting for a read, and never for the next mouse move.
    fn type_text(&mut self, text: &str) -> Frame {
        let mut last = self.look();
        for ch in text.chars() {
            last = self.frame(vec![egui::Event::Text(ch.to_string())]);
        }
        let _ = last;
        self.look()
    }

    /// Focus the box and type `text` from empty.
    fn type_into_the_box(&mut self, text: &str) -> Frame {
        self.focus_the_box();
        self.type_text(text)
    }

    /// Press the `Clear` button, and return the frame that saw it.
    fn clear(&mut self) -> Frame {
        let at = {
            let painted = self.look();
            painted.at("Clear")
        };
        let clicked = self.click(at);
        let _ = clicked;
        self.look()
    }
}

/// The search box narrows the list to the matching stories, in the frame the character
/// lands, and clearing it brings every story back.
///
/// This is the reported bug at its narrowest: four rows handed in, `"chip"` in the box,
/// and — before the fix — four rows painted.
#[test]
fn typing_a_needle_narrows_the_list_and_clearing_it_restores_every_story() {
    let temp = TempArchive::build();
    let archive = Archive::open(&temp.file).expect("the fixture, read-only");
    let mut page = ArticlesPage::new(&archive, WIDE);

    let first = page.look();
    assert_eq!(
        first.stories(),
        TITLES.to_vec(),
        "the page does not open on every story of the window:\n{}",
        first.text
    );
    assert!(
        first.paints("4 stories in the last 168 h, newest first"),
        "the heading with an empty box does not count the window:\n{}",
        first.text
    );

    let typed = page.type_into_the_box("chip");
    assert_eq!(
        typed.stories(),
        vec!["Chip export rules tighten in Asia"],
        "a needle in the box did not narrow the list it is drawn above:\n{}",
        typed.text
    );
    for marker in MARKERS {
        assert!(
            !typed.paints(marker),
            "the pane painted {marker} while the list was narrowed to one story:\n{}",
            typed.text
        );
    }
    assert!(
        typed.paints("1 of 4 stories match, in the last 168 h"),
        "the heading does not count the rows the list is showing:\n{}",
        typed.text
    );

    // Clearing the box restores every story — through the button, as a reader clears it.
    let cleared = page.clear();
    assert_eq!(
        cleared.stories(),
        TITLES.to_vec(),
        "clearing the search did not bring the window back:\n{}",
        cleared.text
    );
    assert!(
        cleared.paints("4 stories in the last 168 h, newest first"),
        "the heading did not go back to the window's own count:\n{}",
        cleared.text
    );
}

/// The story the reader has open keeps its reading pane while the search narrows onto it,
/// and says a sentence of its own — not "no longer in the window" — when the search is
/// what is hiding it.
///
/// Before the fix the first keystroke dropped the selection (`view.selected = None`), so
/// the pane that had been reading a body was replaced by "Choose a story on the left":
/// a filter that answered "nothing" about the one story on screen.
#[test]
fn the_open_story_keeps_its_reader_and_says_when_the_search_hides_it() {
    let temp = TempArchive::build();
    let archive = Archive::open(&temp.file).expect("the fixture, read-only");
    let mut page = ArticlesPage::new(&archive, WIDE);
    page.view.selected = Some(1);
    let opened = page.look();
    assert_eq!(
        opened.markers(),
        vec!["PANE-CHIP"],
        "the pane did not open the selected story:\n{}",
        opened.text
    );

    // A needle that still selects it: the pane must keep painting the body.
    let narrowed = page.type_into_the_box("chip");
    assert_eq!(
        narrowed.markers(),
        vec!["PANE-CHIP"],
        "narrowing the search onto the open story emptied its reader:\n{}",
        narrowed.text
    );
    assert!(
        !narrowed.paints("Choose a story on the left"),
        "the keystroke threw the open story away:\n{}",
        narrowed.text
    );

    // A needle that no longer selects it: one sentence, and it is the true one.
    let hidden = page.type_text("z");
    assert!(
        hidden.markers().is_empty(),
        "the pane kept painting a story the search hides:\n{}",
        hidden.text
    );
    assert!(
        hidden.paints("The open story is not in this search. Clear the search to read it."),
        "the search hid the open story and the pane said something else:\n{}",
        hidden.text
    );
    assert!(
        !hidden.paints("no longer in the window"),
        "the pane blamed the archive for what the search did:\n{}",
        hidden.text
    );
    assert!(
        hidden.paints("Nothing matches"),
        "the list said nothing at all about a needle that matches nothing:\n{}",
        hidden.text
    );
    assert_eq!(
        hidden.stories(),
        Vec::<&str>::new(),
        "the list still painted stories the needle does not match:\n{}",
        hidden.text
    );
}

/// A needle with no match says the empty sentence rather than nothing at all, and the
/// heading says zero rather than the number of rows it was handed.
#[test]
fn a_needle_that_matches_nothing_says_the_empty_sentence() {
    let temp = TempArchive::build();
    let archive = Archive::open(&temp.file).expect("the fixture, read-only");
    let mut page = ArticlesPage::new(&archive, WIDE);
    let typed = page.type_into_the_box("zzzqqq");
    assert_eq!(
        typed.stories(),
        Vec::<&str>::new(),
        "a needle the archive does not hold answered with rows:\n{}",
        typed.text
    );
    assert!(
        typed.paints("Nothing matches. Clear the search, widen the window, or collect a story."),
        "an empty list said nothing:\n{}",
        typed.text
    );
    assert!(
        typed.paints("0 of 4 stories match, in the last 168 h"),
        "the heading does not say the list is empty:\n{}",
        typed.text
    );
    assert!(
        typed.paints("Choose a story on the left to read it here."),
        "the pane has no sentence for a search that hides everything:\n{}",
        typed.text
    );
}

/// The needle is matched in the three columns the list can show a hit in — the same three
/// `crate::data::ARTICLE_SEARCH` searches — and against the reader's case.
///
/// A view that searched only titles would hide a story the archive's query returned, and
/// that is the one direction a display filter must never take.
#[test]
fn the_view_matches_the_same_three_columns_the_query_does() {
    let temp = TempArchive::build();
    let archive = Archive::open(&temp.file).expect("the fixture, read-only");
    let mut page = ArticlesPage::new(&archive, WIDE);

    // "tighten" is in the title, "sentence" only in every summary, "marker body" only in
    // the stories' `content` — one column each.
    for (needle, expected) in [
        ("TIGHTEN", vec!["Chip export rules tighten in Asia"]),
        ("summary sentence", TITLES.to_vec()),
        ("PANE-TIMBER: the body", vec!["Timber prices settle on the coast"]),
    ] {
        let typed = page.type_into_the_box(needle);
        assert_eq!(
            typed.stories(),
            expected,
            "the needle {needle:?} did not select the rows the query selects:\n{}",
            typed.text
        );
        // clear between needles, through the button
        let _ = page.clear();
    }
}

// ---------------------------------------------------------------------------
// (5–7) the real window: `Atlas::with_archive` + `Atlas::draw`
// ---------------------------------------------------------------------------

/// A window on this fixture, showing Articles, with its first frame drawn.
fn window(temp: &TempArchive) -> (egui::Context, Atlas) {
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.file.clone());
    atlas.set_page(Page::Articles);
    let painted = app_frame(&ctx, &mut atlas, Vec::new());
    assert!(
        painted.paints("Articles"),
        "the window did not open the page asked for:\n{}",
        painted.text
    );
    (ctx, atlas)
}

fn app_frame(ctx: &egui::Context, atlas: &mut Atlas, events: Vec<egui::Event>) -> Frame {
    app_frame_at(ctx, atlas, events, SCREEN)
}

fn app_frame_at(
    ctx: &egui::Context,
    atlas: &mut Atlas,
    events: Vec<egui::Event>,
    screen: Vec2,
) -> Frame {
    let output = ctx.run_ui(input_at(events, screen), |ui| atlas.draw(ui));
    let painted = record(&output.shapes);
    output.drop_without_applying_deltas();
    painted
}

fn app_click(ctx: &egui::Context, atlas: &mut Atlas, at: Pos2) -> Frame {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    app_frame(ctx, atlas, vec![egui::Event::PointerMoved(at), button(true)]);
    app_frame(ctx, atlas, vec![egui::Event::PointerMoved(at), button(false)])
}

/// Type into the window's search box, one character per frame.
fn app_type(ctx: &egui::Context, atlas: &mut Atlas, text: &str) -> Frame {
    let hint = "a name, a place, a phrase";
    let painted = app_frame(ctx, atlas, Vec::new());
    let at = painted.at(hint);
    app_click(ctx, atlas, at);
    let mut last = app_frame(ctx, atlas, Vec::new());
    for ch in text.chars() {
        last = app_frame(ctx, atlas, vec![egui::Event::Text(ch.to_string())]);
    }
    last
}

/// The window's list holds what the reader searched for and nothing else, even though the
/// app appends one story per map pin to every read.
///
/// Measured before the fix on this fixture: typing `chip` into the window's own box
/// painted "Chip export rules tighten in Asia", "Harbour crane strike ends" and "Timber
/// prices settle on the coast" — one match and two rows the reader did not ask for, from
/// `Atlas::reload`'s pin top-up, which reads `pins` without the needle.
#[test]
fn the_window_does_not_keep_pin_stories_under_a_search() {
    let temp = TempArchive::build();
    let (ctx, mut atlas) = window(&temp);

    let before = app_frame(&ctx, &mut atlas, Vec::new());
    assert_eq!(
        before.stories(),
        vec![TITLES[0], TITLES[1], TITLES[2]],
        "the window does not open on every story of the {HOURS} h window:\n{}",
        before.text
    );

    let typed = app_type(&ctx, &mut atlas, "chip");
    assert_eq!(
        typed.stories(),
        vec!["Chip export rules tighten in Asia"],
        "the window's list kept rows the needle does not match:\n{}",
        typed.text
    );
    assert!(
        !typed.paints("Harbour crane strike ends") && !typed.paints("Timber prices settle on the coast"),
        "a pin's story came back through a search:\n{}",
        typed.text
    );

    // …and it settles the same way a frame later, when the read for the new needle has
    // happened: no row appears late, and none disappears.
    let settled = app_frame(&ctx, &mut atlas, Vec::new());
    assert_eq!(
        settled.stories(),
        vec!["Chip export rules tighten in Asia"],
        "the settled frame after the read disagrees with the frame that typed:\n{}",
        settled.text
    );
}

/// Choosing a window in the rail re-reads the archive and repaints the list in the same
/// frame — the rows of the previous window are never left on screen.
#[test]
fn changing_the_window_rereads_and_repaints_the_list() {
    let temp = TempArchive::build();
    let (ctx, mut atlas) = window(&temp);

    let before = app_frame(&ctx, &mut atlas, Vec::new());
    assert!(
        before.paints(&format!("3 stories in the last {HOURS} h, newest first")),
        "the window did not open on the 3 d list:\n{}",
        before.text
    );
    assert_eq!(
        before.stories(),
        vec![TITLES[0], TITLES[1], TITLES[2]],
        "the 3 d list is not the three newest stories:\n{}",
        before.text
    );

    // 24 h: two stories, and the frame that saw the click is the frame that shows them.
    let at = before.at("24 h");
    let clicked = app_click(&ctx, &mut atlas, at);
    assert!(
        clicked.paints("2 stories in the last 24 h, newest first"),
        "the click frame still showed the previous window:\n{}",
        clicked.text
    );
    assert_eq!(
        clicked.stories(),
        vec![TITLES[0], TITLES[1]],
        "the 24 h list is not the two stories of the last day:\n{}",
        clicked.text
    );
    assert!(
        !clicked.paints(TITLES[2]) && !clicked.paints(TITLES[3]),
        "stories outside 24 h stayed in the list:\n{}",
        clicked.text
    );

    // 7 d: all four, including the 60 h story that neither narrower window held.
    let at = clicked.at("7 d");
    let wide = app_click(&ctx, &mut atlas, at);
    assert!(
        wide.paints("4 stories in the last 168 h, newest first"),
        "the wider window did not re-read the list:\n{}",
        wide.text
    );
    assert_eq!(
        wide.stories(),
        TITLES.to_vec(),
        "the 7 d list is not every story of the fixture:\n{}",
        wide.text
    );

    // The window is a demand, not a filter on the rows in hand: the status row counts the
    // reads, so a repaint that never re-read is visible as well.
    let reads = wide
        .labels()
        .iter()
        .find(|label| label.contains(" read"))
        .map(|label| label.to_string())
        .unwrap_or_else(|| panic!("the status row did not report its reads:\n{}", wide.text));
    assert!(
        reads.contains(" 3 reads") || reads.contains(" 4 reads") || reads.contains(" 5 reads"),
        "three window changes cost {} — the list was re-read, so the count must rise: {:?}",
        reads,
        wide.labels()
    );
}

/// A window change keeps the search: choosing 24 h while a needle is in the box narrows
/// within the new window, and widening again does not smuggle the needle away.
///
/// This is the sentence the reader used — "I choose a filter and nothing comes" — read the
/// other way round: after a window change, the list still answers the box above it.
#[test]
fn a_window_change_keeps_the_search_that_narrows_it() {
    let temp = TempArchive::build();
    let (ctx, mut atlas) = window(&temp);
    let typed = app_type(&ctx, &mut atlas, "chip");
    assert_eq!(
        typed.stories(),
        vec![TITLES[0]],
        "the search did not narrow the list:\n{}",
        typed.text
    );

    let at = typed.at("24 h");
    let clicked = app_click(&ctx, &mut atlas, at);
    assert_eq!(
        clicked.stories(),
        vec![TITLES[0]],
        "narrowing the window lost the search:\n{}",
        clicked.text
    );
    assert!(
        clicked.paints("1 of 1 stories match, in the last 24 h, newest first")
            || clicked.paints("1 of 2 stories match, in the last 24 h, newest first"),
        "the heading does not say what the list holds after a window change:\n{}",
        clicked.text
    );

    // The story outside the new window is gone even from the pane's list, and the search
    // still answers: widen it and the same one story answers again.
    let at = clicked.at("7 d");
    let wide = app_click(&ctx, &mut atlas, at);
    assert_eq!(
        wide.stories(),
        vec![TITLES[0]],
        "widening the window dropped the search and showed everything:\n{}",
        wide.text
    );
    assert!(
        wide.paints("1 of 4 stories match, in the last 168 h, newest first"),
        "the heading after widening does not count the window's rows:\n{}",
        wide.text
    );
}

// ---------------------------------------------------------------------------
// the layout: one page in two shapes, chosen by the page's own width
// ---------------------------------------------------------------------------

/// The page width below which the view stacks instead of splitting — the rule the view
/// documents on its own `SPLIT_FLOOR`. It is repeated here because it is the *spec*: a page
/// under it must stack, a page over it must split, and a test that asked the view which
/// shape it chose could not tell a choice from a fallback.
const SPLIT_FLOOR: f32 = 820.0;

/// The four sizes the layout audit measured, as windows (`src/main.rs` opens 1440×900; a
/// reader may open anything).
const AUDIT_SIZES: [(f32, f32); 4] = [
    (700.0, 520.0),
    (900.0, 620.0),
    (1280.0, 820.0),
    (1680.0, 1200.0),
];

/// The page a window of this size hands to `articles::show`: the window, minus the rail
/// and the page's margins. The rail folds below its own threshold, so this is read off the
/// frame rather than assumed — at 700 the rail is 64 px of marks, at 1280 it
/// is 188 px of words.
fn page_inner(frame: &Frame) -> Rect {
    // The page's own background: the widest filled rectangle that starts at the window's
    // top and not at x = 0 (the rail has that strip). Its inner box is the page.
    let page = frame
        .fills
        .iter()
        .copied()
        .filter(|rect| rect.top() == 0.0 && rect.width() > 300.0)
        .max_by(|a, b| a.width().total_cmp(&b.width()))
        .unwrap_or_else(|| panic!("the frame painted no page background: {:?}", frame.fills));
    // …minus the app's page margin, `symmetric(20, 16)` (`src/app.rs`).
    page.shrink2(Vec2::new(20.0, 16.0))
}

/// Where a run of the page is clipped: a list row's clip is the list's viewport, the body's
/// is the pane's, and their relationship is what tells the two layouts apart.
fn clip_of<'a>(frame: &'a Frame, text: &str) -> Rect {
    frame
        .runs
        .iter()
        .find(|(_, _, run)| run == text)
        .map(|(_, clip, _)| *clip)
        .unwrap_or_else(|| {
            panic!(
                "the frame did not paint {text:?}; it painted {:?}",
                frame.labels()
            )
        })
}

/// Where a run of the page is painted.

/// The body of the open story, which is long enough to wrap: its rectangle is the reading
/// column's width, and its clip is the pane's viewport.
fn body_run(frame: &Frame) -> (Rect, Rect) {
    frame
        .runs
        .iter()
        .find(|(_, _, text)| text.starts_with("PANE-CHIP"))
        .map(|(rect, clip, _)| (*rect, *clip))
        .unwrap_or_else(|| panic!("the pane painted no story at all:\n{}", frame.text))
}

/// A window on the fixture at `screen`, showing Articles, with the first story open.
///
/// Nothing is assumed about the layout: the row is found where the list painted its title
/// and clicked there, the same path a reader takes.
fn opened_window(screen: Vec2) -> (TempArchive, egui::Context, Atlas) {
    let temp = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.file.clone());
    atlas.set_page(Page::Articles);
    let _ = app_frame_at(&ctx, &mut atlas, Vec::new(), screen);
    let settled = app_frame_at(&ctx, &mut atlas, Vec::new(), screen);
    assert!(
        settled.stories().contains(&TITLES[0]),
        "the list did not paint its first story:\n{}",
        settled.text
    );
    let at = settled.at(TITLES[0]);
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    app_frame_at(&ctx, &mut atlas, vec![egui::Event::PointerMoved(at), button(true)], screen);
    app_frame_at(&ctx, &mut atlas, vec![egui::Event::PointerMoved(at), button(false)], screen);
    let _ = app_frame_at(&ctx, &mut atlas, Vec::new(), screen);
    (temp, ctx, atlas)
}

/// The audit's complaint, pinned: at every window the layout audit measured, the reading
/// pane has a column of at least 260 px (or the page is stacked, which gives it the whole
/// width), the list is at least 280 px wide, and no text run on the page is clipped
/// horizontally by more than 2 px.
///
/// Measured before the fix, at 700×520: the list was 471 of 473 px and **no pane was
/// painted at all** (the body run did not exist, which is what `body_run`'s panic says); at
/// 900×620 the body was 152 px wide. The cause was `default_size(520.0)` — one number for
/// every screen.
#[test]
fn the_reading_pane_keeps_a_readable_column_at_every_audited_size() {
    for (w, h) in AUDIT_SIZES {
        let screen = Vec2::new(w, h);
        let (_temp, _ctx, _atlas) = opened_window(screen);
        // `opened_window` returns the window so the archive lives as long as the frames;
        // the last frame is drawn again here on the same context for the measurement.
        let (_temp, ctx, mut atlas) = (_temp, _ctx, _atlas);
        let painted = app_frame_at(&ctx, &mut atlas, Vec::new(), screen);
        let page = page_inner(&painted);
        let (body, body_clip) = body_run(&painted);
        let row_clip = clip_of(&painted, TITLES[0]);
        let stacked = page.width() < SPLIT_FLOOR;

        assert!(
            body.width() >= 260.0,
            "at {w}×{h} the reading column is {} px wide (page {} px):\n{}",
            body.width(),
            page.width(),
            painted.text
        );
        assert!(
            row_clip.width() >= 280.0,
            "at {w}×{h} the list is {} px wide (page {} px):\n{}",
            row_clip.width(),
            page.width(),
            painted.text
        );
        if stacked {
            assert!(
                body_clip.top() >= row_clip.bottom() - 1.0,
                "at {w}×{h} a {} px page must stack, and the story is beside the list \
                 (body clip {body_clip:?}, list clip {row_clip:?})",
                page.width()
            );
            assert!(
                row_clip.width() >= page.width() - 24.0,
                "at {w}×{h} the stacked list does not take the page's width: {} of {}",
                row_clip.width(),
                page.width()
            );
        } else {
            assert!(
                body_clip.left() >= row_clip.right() - 1.0,
                "at {w}×{h} a {} px page must keep two columns, and the story is not beside \
                 the list (body clip {body_clip:?}, list clip {row_clip:?})",
                page.width()
            );
            // The share, and its floor and ceiling: the split is what was fixed, so the
            // split numbers are asserted and not merely the outcome.
            let share = row_clip.width() / page.width();
            assert!(
                (0.38..=0.48).contains(&share),
                "at {w}×{h} the list is {:.0} % of the page, not about 45 %",
                share * 100.0
            );
            assert!(
                (300.0..=660.0).contains(&row_clip.width()),
                "at {w}×{h} the list is {} px, outside the [320, 640] band its floor and \
                 ceiling set",
                row_clip.width()
            );
        }

        // The clipping rule, for every run the page painted. The status row's own labels
        // are not page runs: they belong to `src/app.rs`, and one of them overflows by
        // 133 px at every size (reported, measured on three pages this task does not own).
        for (rect, clip, text) in &painted.runs {
            if !page.contains(rect.center()) {
                continue;
            }
            assert!(
                rect.right() <= clip.right() + 2.0,
                "at {w}×{h} a run is clipped by {} px: {:?} ({:?} in {:?})",
                rect.right() - clip.right(),
                text.chars().take(40).collect::<String>(),
                rect,
                clip
            );
        }

        // And the open story is on screen at all, which is the thing the audit found
        // missing at 700×520.
        assert!(
            painted.paints("PANE-CHIP"),
            "at {w}×{h} the open story is not painted:\n{}",
            painted.text
        );
    }
}

/// The same page at 700×520 says which shape it took, so a regression cannot pass by
/// quietly painting two narrow columns where the reader needs one.
#[test]
fn the_narrow_page_stacks_the_list_above_the_story() {
    let (_temp, ctx, mut atlas) = opened_window(Vec2::new(700.0, 520.0));
    let painted = app_frame_at(&ctx, &mut atlas, Vec::new(), Vec2::new(700.0, 520.0));
    let page = page_inner(&painted);
    let (_, body_clip) = body_run(&painted);
    let row_clip = clip_of(&painted, TITLES[0]);
    assert!(
        page.width() < SPLIT_FLOOR,
        "the fixture's page is {} px wide at a 700 px window",
        page.width()
    );
    assert!(
        body_clip.top() >= row_clip.bottom() - 1.0,
        "the story is not below the list at 700×520: list {row_clip:?}, body {body_clip:?}"
    );
    // The list is bounded, or the story would be a sliver under a list that ate the page.
    assert!(
        row_clip.height() <= 260.0 + 1.0,
        "the stacked list is {} px tall, above its ceiling",
        row_clip.height()
    );
    assert!(
        body_clip.height() >= 100.0,
        "the story got {} px of height under the list",
        body_clip.height()
    );
}


// ---------------------------------------------------------------------------
// the row is one door: title, meta, summary and the hairline all open it
// ---------------------------------------------------------------------------

/// The air under a row's text, which holds its hairline — the same `theme::space::SM` the
/// view uses. Repeated here so the tests aim at the band the view documents, not at a
/// number that happens to work.
const ROW_AIR: f32 = 8.0;

/// The first painted run whose text contains `needle` and whose top is at or below `under`:
/// a row's meta line and summary are found by where they sit, not by what they say, because
/// all four fixture rows say the same thing.
fn first_line_after(frame: &Frame, needle: &str, under: f32) -> Rect {
    frame
        .runs
        .iter()
        .filter(|(rect, _, text)| text.contains(needle) && rect.top() >= under - 0.5)
        .map(|(rect, _, _)| *rect)
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .unwrap_or_else(|| {
            panic!(
                "no line holding {needle:?} at or below y = {under}: {:?}",
                frame
                    .runs
                    .iter()
                    .map(|(rect, _, text)| (rect.top(), text.as_str()))
                    .collect::<Vec<_>>()
            )
        })
}

/// The three lines of the first row, top to bottom.
fn first_row(frame: &Frame) -> (Rect, Rect, Rect) {
    let title = frame.runs.iter()
        .find(|(_, _, text)| text == TITLES[0])
        .map(|(rect, _, _)| *rect)
        .unwrap_or_else(|| panic!("the list painted no title:\n{}", frame.text));
    let meta = first_line_after(frame, "Madrid Desk", title.bottom());
    let summary = first_line_after(frame, "a summary sentence", meta.bottom());
    (title, meta, summary)
}

/// A window on this file's archive, showing Articles, with its first two frames drawn —
/// everything the row tests need to know where the list painted a row.
fn open_window(screen: Vec2) -> (TempArchive, egui::Context, Atlas, Frame) {
    let temp = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.file.clone());
    atlas.set_page(Page::Articles);
    let _ = app_frame_at(&ctx, &mut atlas, Vec::new(), screen);
    let painted = app_frame_at(&ctx, &mut atlas, Vec::new(), screen);
    (temp, ctx, atlas, painted)
}

/// A click at `at` in a *fresh* window opens the first story: the window starts with nothing
/// open, so what the click did cannot be confused with what was already there.
///
/// A fresh window per point is also why this is a real test of the band and not of the
/// view's state: the page's own `selected` is not reachable from outside the crate, so the
/// window is reset the only honest way — by opening another one.
fn a_click_there_opens_the_first_story(at: Pos2, what: &str) {
    let (_temp, ctx, mut atlas, painted) = open_window(SCREEN);
    assert!(
        painted.paints("Choose a story on the left to read it here."),
        "the window did not start with nothing open:\n{}",
        painted.text
    );
    app_click(&ctx, &mut atlas, at);
    let after = app_frame(&ctx, &mut atlas, Vec::new());
    assert!(
        after.paints("PANE-CHIP"),
        "a click on the {what} at {at:?} opened nothing:\n{}",
        after.text
    );
}

/// The title, the `10:53 · SOURCE` line, the summary under it and the hairline that closes
/// the row all open the same story — the audit's finding, answered.
#[test]
fn any_line_of_a_row_opens_its_story() {
    let (_temp, _ctx, _atlas, painted) = open_window(SCREEN);
    let (title, meta, summary) = first_row(&painted);

    let targets = [
        ("title", Pos2::new(title.left() + 20.0, title.center().y)),
        ("meta line", Pos2::new(meta.left() + 20.0, meta.center().y)),
        ("summary", Pos2::new(summary.left() + 20.0, summary.center().y)),
        // The hairline lives in the air under the summary, and the band ends there.
        (
            "hairline",
            Pos2::new(title.left() + 20.0, summary.bottom() + ROW_AIR / 2.0),
        ),
        // …and the band's own left edge, which is neither ink nor a line: a row is a band,
        // not a text run.
        ("left edge of the band", Pos2::new(title.left() + 1.0, meta.center().y)),
    ];
    for (what, at) in targets {
        a_click_there_opens_the_first_story(at, what);
    }
}

/// The audit's own measurement, reproduced through the real window: a 4 px sweep down the
/// first row, in a fresh window per point, counting how many points answer.
///
/// Before this pass the row answered on six of eighteen points (33 %) — the title line only,
/// dead from 24 px below the title, with the meta line at 28 and the summary at 50. The sweep
/// prints its own numbers so the before and after can be compared, and asserts them so a
/// regression cannot quietly make half a row dead again.
#[test]
fn a_four_pixel_sweep_down_a_row_answers_at_every_point() {
    let (_temp, _ctx, _atlas, painted) = open_window(SCREEN);
    let (title, meta, summary) = first_row(&painted);

    let top = title.top();
    let bottom = summary.bottom() + ROW_AIR;
    let x = title.left() + 24.0;
    let mut answered = 0;
    let mut asked = 0;
    let mut dead = Vec::new();
    let mut y = top;
    while y < bottom {
        let (_temp, ctx, mut atlas, _) = open_window(SCREEN);
        app_click(&ctx, &mut atlas, Pos2::new(x, y));
        let after = app_frame(&ctx, &mut atlas, Vec::new());
        asked += 1;
        if after.paints("PANE-CHIP") {
            answered += 1;
        } else {
            dead.push(y - top);
        }
        y += 4.0;
    }
    println!(
        "MEASURED row sweep: {answered}/{asked} points answer ({:.0} %) from the title top to \
         the hairline; dead offsets {dead:?}",
        100.0 * answered as f32 / asked as f32
    );
    assert!(
        dead.is_empty(),
        "{answered} of {asked} points of the row band answer; dead at {dead:?} px below the \
         title, with the meta line at {:.0} and the summary at {:.0}",
        meta.top() - top,
        summary.top() - top
    );
}

/// The focus ring is the row's: it covers the title, the meta line, the summary and the
/// hairline — not the title alone, which is where it used to be drawn.
#[test]
fn the_focus_ring_covers_the_whole_row() {
    let (_temp, ctx, mut atlas, painted) = open_window(SCREEN);
    let (title, meta, summary) = first_row(&painted);
    // Open the row (and so give it focus).
    app_click(&ctx, &mut atlas, Pos2::new(title.left() + 20.0, title.center().y));
    let focused = app_frame(&ctx, &mut atlas, Vec::new());

    // The window paints more than one outline (the rail's edge, the page's hairline), so the
    // ring is found by its weight: the theme's `FOCUS_WIDTH`, and the largest such rect.
    let (ring, width) = focused
        .outlines
        .iter()
        .copied()
        .filter(|(_, width)| (*width - theme::FOCUS_WIDTH).abs() < 0.01)
        .max_by(|(a, _), (b, _)| (a.width() * a.height()).total_cmp(&(b.width() * b.height())))
        .unwrap_or_else(|| {
            panic!(
                "the focused row painted no {} px ring; outlines were {:?}",
                theme::FOCUS_WIDTH,
                focused.outlines
            )
        });
    assert!(
        (width - theme::FOCUS_WIDTH).abs() < 0.01,
        "the ring is {width} px, not the theme's {}",
        theme::FOCUS_WIDTH
    );
    assert!(
        ring.top() <= title.top() + 0.5 && ring.bottom() >= summary.bottom() + ROW_AIR - 0.5,
        "the ring {ring:?} does not cover the row (title {title:?}, summary {summary:?})"
    );
    assert!(
        ring.contains_rect(meta),
        "the ring {ring:?} does not cover the meta line {meta:?}"
    );
    // …and it is as wide as the row, not as wide as the title. It stops where the scroll
    // bar begins (a scroll bar is not part of the row it scrolls), so the comparison is
    // against the row's own band, not against the scroll area's viewport.
    let row_width = clip_of(&focused, TITLES[0]).width();
    assert!(
        ring.left() <= title.left() + 0.5 && ring.width() >= row_width - 24.0,
        "the ring is {} px wide from {:.0} in a {row_width} px row: it does not span it",
        ring.width(),
        ring.left()
    );
    println!(
        "MEASURED focus ring: {:.0}×{:.0} px around a {row_width:.0} px row",
        ring.width(),
        ring.height()
    );
}

/// `Clear` is only offered when there is something to clear: an enabled button with nothing
/// to do is a dead click target, which is what this pass exists to remove.
#[test]
fn clear_is_gone_when_the_search_box_is_empty() {
    let temp = TempArchive::build();
    let archive = Archive::open(&temp.file).expect("the fixture, read-only");
    let mut page = ArticlesPage::new(&archive, WIDE);

    let empty_box = page.look();
    assert!(
        !empty_box.paints("Clear"),
        "an empty search box still offers Clear:\n{}",
        empty_box.text
    );

    let typed = page.type_into_the_box("chip");
    assert!(
        typed.paints("Clear"),
        "a search box holding \"chip\" offers no Clear:\n{}",
        typed.text
    );

    // …and clearing leaves the box empty, so the button goes away again.
    let cleared = page.clear();
    assert!(
        page.view.needle.is_empty(),
        "Clear did not empty the box: {:?}",
        page.view.needle
    );
    assert!(
        !cleared.paints("Clear"),
        "Clear stayed after it emptied the box:\n{}",
        cleared.text
    );
}

// ---------------------------------------------------------------------------
// the keyboard takes the same path as the mouse
// ---------------------------------------------------------------------------

fn key_event(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

/// One key press, then the frame the press's effect is drawn in.
fn app_key(ctx: &egui::Context, atlas: &mut Atlas, key: egui::Key) -> Frame {
    let pressed = app_frame(ctx, atlas, vec![key_event(key, true)]);
    let _ = pressed;
    let _ = app_frame(ctx, atlas, vec![key_event(key, false)]);
    app_frame(ctx, atlas, Vec::new())
}

/// ↓ moves the open story down one, Enter leaves it open, and the result is the frame a
/// mouse click on that row would have produced.
#[test]
fn the_arrow_keys_and_enter_match_a_mouse_click() {
    let temp = TempArchive::build();
    let (ctx, mut atlas) = window(&temp);
    let settled = app_frame(&ctx, &mut atlas, Vec::new());
    let at = settled.at(TITLES[0]);
    app_click(&ctx, &mut atlas, at);
    let opened = app_frame(&ctx, &mut atlas, Vec::new());
    assert!(
        opened.paints("PANE-CHIP"),
        "the click did not open the first story:\n{}",
        opened.text
    );

    let after_down = app_key(&ctx, &mut atlas, egui::Key::ArrowDown);
    assert!(
        after_down.paints("PANE-HARBOUR") && !after_down.paints("PANE-CHIP"),
        "↓ did not move the open story to the second row:\n{}",
        after_down.text
    );

    let after_enter = app_key(&ctx, &mut atlas, egui::Key::Enter);
    assert!(
        after_enter.paints("PANE-HARBOUR"),
        "Enter closed or moved the story it was pressed on:\n{}",
        after_enter.text
    );

    // The same story, opened the way a reader with a mouse opens it: the two frames must
    // agree about what is open, and about the row that wears the selection.
    let (mouse_ctx, mut mouse) = window(&temp);
    let settled = app_frame(&mouse_ctx, &mut mouse, Vec::new());
    let at = settled.at(TITLES[1]);
    app_click(&mouse_ctx, &mut mouse, at);
    let by_mouse = app_frame(&mouse_ctx, &mut mouse, Vec::new());

    for marker in MARKERS {
        assert_eq!(
            after_enter.paints(marker),
            by_mouse.paints(marker),
            "the keyboard and the mouse disagree about {marker}: \\nkeyboard:\\n{}\\nmouse:\\n{}",
            after_enter.text,
            by_mouse.text
        );
    }
    assert!(
        by_mouse.paints("PANE-HARBOUR"),
        "the mouse did not open the second story either:\n{}",
        by_mouse.text
    );
}

/// ↑ moves back up, and stops at the first row instead of wrapping to the last: a list that
/// wraps turns one keystroke too many into a page jump.
#[test]
fn the_up_key_moves_back_and_stops_at_the_top() {
    let temp = TempArchive::build();
    let (ctx, mut atlas) = window(&temp);
    let settled = app_frame(&ctx, &mut atlas, Vec::new());
    let at = settled.at(TITLES[1]);
    app_click(&ctx, &mut atlas, at);
    let _ = app_frame(&ctx, &mut atlas, Vec::new());

    let up = app_key(&ctx, &mut atlas, egui::Key::ArrowUp);
    assert!(
        up.paints("PANE-CHIP"),
        "↑ did not move the open story up one:\n{}",
        up.text
    );
    let again = app_key(&ctx, &mut atlas, egui::Key::ArrowUp);
    assert!(
        again.paints("PANE-CHIP"),
        "↑ on the first row left the first row open:\n{}",
        again.text
    );
}
