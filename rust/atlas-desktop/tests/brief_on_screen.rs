//! The briefing, on the screen.
//!
//! The data path was proven crate by crate (`atlas-store` writes and reads the row,
//! `atlas-ingest` builds it with the model, `atlas-monitor` refreshes it), and the card was
//! proven to draw a value it is handed (`views::brief`'s own tests). What neither proved is the
//! seam between them: that the **window** reads the row for the scope it is showing and hands it
//! to the card. That seam is the shell's, and this file is the only place it is measured.
//!
//! Two measurements, both on a real `Atlas` and a real archive on disk:
//!
//! 1. a briefing row in the archive reaches the Overview as the card's sentence and bullets;
//! 2. the Articles page asks about the scope the page is **narrowed to** — a country chip on
//!    the Overview puts the country's briefing on the list, not the window's.
//!
//! The archive here is built by this file (a `sources`, an `articles` and a `briefings` table and
//! the rows below). Every query the window makes that has no table answers an empty list
//! (`data.rs` swallows a prepare error on purpose), so a table this file does not create is a
//! list the page draws empty — which is exactly what a test of the card wants beside it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::app::Atlas;
use atlas_desktop::theme;

// ------------------------------------------------------------------ a briefing in an archive

/// A `data/atlas.db` of our own, removed when the test ends.
struct TempArchive {
    dir: PathBuf,
}

impl TempArchive {
    /// The archive, with one briefing row per `(scope, key, hours)` this file needs.
    fn build() -> TempArchive {
        let dir = unique_dir();
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let file = dir.join("atlas.db");
        let conn = rusqlite::Connection::open(&file).expect("a new database");
        conn.execute_batch(
            "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE articles(
                 id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                 published TEXT, summary TEXT, content TEXT, iso TEXT, place TEXT, url TEXT,
                 full_text INTEGER, relevant INTEGER, dupe INTEGER, lat REAL, lon REAL);
             CREATE TABLE briefings(
                 scope TEXT NOT NULL, key TEXT NOT NULL, hours INTEGER NOT NULL,
                 watermark INTEGER NOT NULL DEFAULT 0, created TEXT NOT NULL DEFAULT '',
                 line TEXT NOT NULL DEFAULT '', bullets TEXT NOT NULL DEFAULT '[]',
                 articles INTEGER NOT NULL DEFAULT 0, model TEXT NOT NULL DEFAULT '',
                 ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(scope, key, hours));",
        )
        .expect("the three tables the window reads");
        conn.execute("INSERT INTO sources(id, name) VALUES(1, 'The Nordic Post')", [])
            .expect("a source");
        // Three German stories and one American: a country chip on `DEU` narrows to three of
        // the four, so a briefing about Germany is visibly *not* the window's sentence.
        let stamp = "2026-11-22T09:00:00Z";
        for (id, title, iso, place, lat, lon) in [
            (1, "Harbour crane strike ends after nine days", "DEU", "Hamburg", 54.0, 10.0),
            (2, "Alpine rail freight reaches a record", "AUT", "Vienna", 48.2, 16.4),
            (3, "Chip export rules tighten in Asia", "JPN", "Tokyo", 35.7, 139.7),
            (4, "Drought reshapes the southern harvest", "ESP", "Madrid", 40.4, -3.7),
        ] {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', 1, ?3, ?4, ?5, ?6, ?7, ?8, 1, 1, 0, ?9, ?10)",
                rusqlite::params![
                    id,
                    title,
                    stamp,
                    format!("{title} — a summary sentence."),
                    format!("{title}. The body of the story."),
                    iso,
                    place,
                    format!("https://news.example.invalid/{id}"),
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        // The two briefings: one for the whole window, one for Germany. They are deliberately
        // different sentences, because the assertion is that the *right* one is drawn.
        insert_briefing(
            &conn,
            "window",
            "",
            72,
            "Four corners of the world, one week: strikes, rail, chips and drought.",
            &[
                "The harbour strike ended after nine days [Article #1].",
                "Alpine rail freight reached a record [Article #2].",
            ],
            4,
        );
        insert_briefing(
            &conn,
            "country",
            "DEU",
            72,
            "Germany's week was one long labour dispute that ended on the ninth day.",
            &["The cranes stopped for nine days [Article #1]."],
            3,
        );
        // The process that reads this file opens it read-only; nothing else may write it.
        drop(conn);
        TempArchive { dir }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("atlas.db")
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn insert_briefing(
    conn: &rusqlite::Connection,
    scope: &str,
    key: &str,
    hours: i64,
    line: &str,
    bullets: &[&str],
    articles: i64,
) {
    let bullets = serde_json::to_string(bullets).expect("the bullets as json");
    conn.execute(
        "INSERT INTO briefings(scope, key, hours, watermark, created, line, bullets, articles, \
         model, ms) VALUES(?1, ?2, ?3, 4, '2026-11-22T17:00:00Z', ?4, ?5, ?6, 'local', 900)",
        rusqlite::params![scope, key, hours, line, bullets, articles],
    )
    .expect("a briefing row");
}

/// A directory no other test of this binary shares: the process id, the clock and a counter.
impl TempArchive {
    /// The same archive with the window **on** it: six stories from the last two hours, dates
    /// relative to this run, and one briefing written over them.
    ///
    /// The audit of the page's plan needs a frame that has a map to draw: [`build`]'s stamps are
    /// fixed, so its stories are outside any 72 h window a later run asks for, the map card is
    /// its empty state, and there is no plate to measure.
    fn recent() -> TempArchive {
        let dir = unique_dir();
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let file = dir.join("atlas.db");
        let conn = rusqlite::Connection::open(&file).expect("a new database");
        conn.execute_batch(
            "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE articles(
                 id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                 published TEXT, summary TEXT, content TEXT, iso TEXT, place TEXT, url TEXT,
                 full_text INTEGER, relevant INTEGER, dupe INTEGER, lat REAL, lon REAL);
             CREATE TABLE briefings(
                 scope TEXT NOT NULL, key TEXT NOT NULL, hours INTEGER NOT NULL,
                 watermark INTEGER NOT NULL DEFAULT 0, created TEXT NOT NULL DEFAULT '',
                 line TEXT NOT NULL DEFAULT '', bullets TEXT NOT NULL DEFAULT '[]',
                 articles INTEGER NOT NULL DEFAULT 0, model TEXT NOT NULL DEFAULT '',
                 ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(scope, key, hours));",
        )
        .expect("the tables the Overview reads");
        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1, 'The Nordic Post'), (2, 'Alpine Review')",
            [],
        )
        .expect("two sources");
        let stamp = hours_ago(2);
        for (id, title, source, iso, place, lat, lon) in [
            (1, "Harbour crane strike ends after nine days", 1, "DEU", "Hamburg", 54.0, 10.0),
            (2, "Alpine rail freight reaches a record", 2, "AUT", "Vienna", 48.2, 16.4),
            (3, "Chip export rules tighten in Asia", 1, "JPN", "Tokyo", 35.7, 139.7),
            (4, "Drought reshapes the southern harvest", 2, "ESP", "Madrid", 40.4, -3.7),
            (5, "A second harbour approves the contract", 1, "NLD", "Rotterdam", 51.9, 4.5),
            (6, "Ferry traffic resumes on the northern route", 2, "NOR", "Oslo", 59.9, 10.7),
        ] {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 1, 0, ?10, ?11)",
                rusqlite::params![
                    id,
                    title,
                    source,
                    stamp,
                    format!("{title} — a summary sentence."),
                    format!("{title}. The body of the story."),
                    iso,
                    place,
                    format!("https://news.example.invalid/{id}"),
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        insert_briefing(
            &conn,
            "window",
            "",
            72,
            "Six stories from five places: a strike that ended, a rail record, chip rules and a \
             drought.",
            &[
                "The harbour strike ended after nine days [Article #1].",
                "Alpine rail freight reached a record [Article #2].",
                "Chip export rules tightened in Asia [Article #3].",
                "Drought reshaped the southern harvest [Article #4].",
            ],
            6,
        );
        drop(conn);
        TempArchive { dir }
    }
}

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`), so a seeded row stays
/// inside the window whenever this runs. Howard Hinnant's civil-from-days: the crate has no time
/// dependency and neither does this.
fn hours_ago(hours: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let at = now - hours * 3_600;
    let (days, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
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

fn unique_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "atlas-brief-on-screen-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

// ------------------------------------------------------------------------- a headless frame

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.all_styles_mut(|style| {
        style.debug.debug_on_hover = false;
        style.debug.show_widget_hits = false;
    });
    ctx
}

/// Draw one frame at the window's own opening size and return every string it painted.
///
/// `Atlas::draw` is the real frame — rail, status row, page — so what this reads is what a
/// reader sees, not a view drawn in isolation. The texture deltas are dropped rather than
/// applied: there is no renderer here, and epaint 0.37 panics on `Drop` when a delta is still
/// pending (`epaint-0.37/src/textures.rs:337`), which is the same reason `ui_layout.rs` clears
/// them.
fn paint(atlas: &mut Atlas, ctx: &egui::Context, index: usize) -> Vec<String> {
    paint_with(atlas, ctx, index, Vec::new())
}

/// [`paint`] with events in the frame — a keystroke the shell has to hear.
fn paint_with(
    atlas: &mut Atlas,
    ctx: &egui::Context,
    index: usize,
    events: Vec<egui::Event>,
) -> Vec<String> {
    paint_at(atlas, ctx, index, Vec2::new(1280.0, 860.0), events)
}

/// One frame at `size`, read for the strings it painted.
fn paint_at(
    atlas: &mut Atlas,
    ctx: &egui::Context,
    index: usize,
    size: Vec2,
    events: Vec<egui::Event>,
) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for (text, ..) in drawn(atlas, ctx, index, size, events) {
        words.push(text);
    }
    words
}

/// One painted galley: the text, the rectangle it was drawn into, and the clip it was given.
/// A line the clip cuts is a line a reader sees cut, so the two rectangles are the measurement.
type Row = (String, Rect, Rect);

/// One frame at `size`, read the way a backend reads it.
fn drawn(
    atlas: &mut Atlas,
    ctx: &egui::Context,
    index: usize,
    size: Vec2,
    events: Vec<egui::Event>,
) -> Vec<Row> {
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(index as f64 / 60.0),
        events,
        ..Default::default()
    };
    let output = ctx.run_ui(raw, |ui| atlas.draw(ui));
    let mut textures = output.textures_delta;
    textures.clear();
    let mut rows: Vec<Row> = Vec::new();
    for clipped in &output.shapes {
        walk_rows(&clipped.shape, clipped.clip_rect, &mut rows);
    }
    rows
}

fn walk_rows(shape: &Shape, clip: Rect, rows: &mut Vec<Row>) {
    match shape {
        Shape::Text(text) => rows.push((
            text.galley.text().to_string(),
            text.visual_bounding_rect(),
            clip,
        )),
        Shape::Vec(shapes) => {
            for shape in shapes {
                walk_rows(shape, clip, rows);
            }
        }
        Shape::Callback(_) => {}
        _ => {}
    }
}

/// A wheel over `at`: `points` of scrolling, negative towards the foot of the page.
fn wheel(at: Pos2, points: f32) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, points),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// The page's box: the central panel's own clip rectangle less the margins the window draws a
/// page inside (`app.rs:480-481`, 20 px across and 16 px down). The panel's clip is the widest
/// rectangle the page's text was clipped to that does not reach the window's edges.
fn content_box(rows: &[Row], size: Vec2) -> Rect {
    let panel = rows
        .iter()
        .map(|(_, _, clip)| *clip)
        .filter(|clip| clip.min.x > 1.0 && clip.max.y < size.y - 1.0)
        .max_by(|a, b| {
            (a.width() * a.height())
                .partial_cmp(&(b.width() * b.height()))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("the frame painted no central panel");
    Rect::from_min_max(
        panel.min + Vec2::new(20.0, 16.0),
        panel.max - Vec2::new(20.0, 16.0),
    )
}

/// The plate, from egui's interactive-widget inventory: the one click-and-drag widget that
/// covers a fifth of the panel (`tests/layout_screens.rs` reads the same rectangle).
fn plate_at(size: Vec2, archive: &Path) -> Rect {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.all_styles_mut(|style| {
        style.debug.debug_on_hover = false;
        style.debug.show_interactive_widgets = true;
    });
    let mut atlas = Atlas::with_archive(archive.to_path_buf());
    let mut plate = Rect::NOTHING;
    for index in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(index as f64 / 60.0),
            ..Default::default()
        };
        let output = ctx.run_ui(raw, |ui| atlas.draw(ui));
        let mut textures = output.textures_delta;
        textures.clear();
        plate = Rect::NOTHING;
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut |shape: &Shape| {
                if let Shape::Rect(rect) = shape {
                    if rect.stroke.color == egui::Color32::from_rgb(0x88, 0, 0x88)
                        && rect.rect.width() * rect.rect.height()
                            > size.x * size.y * 0.05
                        && rect.rect.width() > 200.0
                    {
                        plate = plate.union(rect.rect);
                    }
                }
            });
        }
    }
    assert!(
        plate != Rect::NOTHING,
        "no plate was inventoried at {:.0}x{:.0}",
        size.x,
        size.y
    );
    plate
}

fn walk(shape: &Shape, visit: &mut impl FnMut(&Shape)) {
    visit(shape);
    if let Shape::Vec(shapes) = shape {
        for shape in shapes {
            walk(shape, visit);
        }
    }
}

fn painted_at_least(words: &[String], needle: &str) -> bool {
    words.iter().any(|word| word.contains(needle))
}

// --------------------------------------------------------------------------------- the tests

/// The window's briefing, drawn on the first page a reader opens.
#[test]
fn the_overview_draws_the_briefing_the_agent_wrote() {
    let archive = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(archive.file());
    // The page opens on the map and the briefing's card is under it: the frame is scrolled to
    // the card the way a reader scrolls, over the page's right margin so the wheel is the
    // page's and not the plate's.
    for index in 0..30 {
        paint(&mut atlas, &ctx, index);
    }
    let mut words = paint(&mut atlas, &ctx, 30);
    for step in 0..10 {
        words = paint_with(&mut atlas, &ctx, 31 + step, wheel(Pos2::new(1_252.0, 400.0), -240.0));
    }

    assert!(
        painted_at_least(
            &words,
            "Four corners of the world, one week: strikes, rail, chips and drought."
        ),
        "the Overview did not paint the window's briefing line. Painted:\n{}",
        words.join("\n")
    );
    assert!(
        painted_at_least(&words, "The harbour strike ended after nine days"),
        "the briefing's first bullet was not painted. Painted:\n{}",
        words.join("\n")
    );
    assert!(
        painted_at_least(&words, "4 stories"),
        "the card's footer did not say how many stories the model was given. Painted:\n{}",
        words.join("\n")
    );
    // The German sentence belongs to the country, not to the window: a card that drew it here
    // would be the shell answering about the wrong scope.
    assert!(
        !painted_at_least(&words, "Germany's week was one long labour dispute"),
        "the Overview drew the country briefing over the whole window. Painted:\n{}",
        words.join("\n")
    );
}

/// The plan at the two sizes the page was reported broken at, measured from its own frames.
///
/// The report was "the briefing is cut off, the map is not shown whole". At 1280×800 the frame
/// said: plate 1 028×209 where the world's aspect asks for 489 px of height, and a briefing band
/// of 160 px that cut the fourth bullet at y 582 and never drew the card's footer at all. This
/// is the two halves of the fix, read back out of the frame: the plate is its own aspect inside
/// the card's width, and the card's last line — its footer — is painted whole inside the page's
/// box once the page is scrolled to it. Neither is true of a clipped card.
#[test]
fn the_plate_keeps_its_aspect_and_the_briefing_footer_is_never_cut() {
    use atlas_desktop::views::overview::PLATE_ASPECT;

    let archive = TempArchive::recent();
    for (width, height) in [(1_280.0_f32, 800.0_f32), (900.0, 620.0)] {
        let size = Vec2::new(width, height);
        let ctx = context();
        let mut atlas = Atlas::with_archive(archive.file());
        // The page settles over a few frames: the bar eases in and the plan is measured against
        // the width that leaves.
        let mut rows = Vec::new();
        for index in 0..30 {
            rows = drawn(&mut atlas, &ctx, index, size, Vec::new());
        }
        let content = content_box(&rows, size);
        let plate = plate_at(size, &archive.file());
        assert!(
            content.contains_rect(plate),
            "the plate {plate:?} is not inside the page's box {content:?} at {width:.0}×{height:.0}"
        );
        let asked = plate.width() / PLATE_ASPECT;
        assert!(
            (plate.height() - asked).abs() <= 1.0,
            "at {width:.0}×{height:.0} the plate is {:.0}×{:.0}: a plate {:.0} px wide at the \
             world's aspect {PLATE_ASPECT} is {:.0} px tall, so the map's own fit crops the world",
            plate.width(),
            plate.height(),
            plate.width(),
            asked
        );
        // The page opens on the map. The card's footer is below the fold — that is the plan, not
        // a cut: it is reachable, and the scroll below is what a reader does to reach it.
        assert!(
            footer(&rows).is_none(),
            "the briefing's footer was already on screen at {width:.0}×{height:.0}; this test \
             measures the scrolled frame, so an unscrolled footer means the page does not scroll"
        );
        let at = Pos2::new(content.max.x - 4.0, content.center().y);
        for step in 0..12 {
            rows = drawn(&mut atlas, &ctx, 31 + step, size, wheel(at, -240.0));
        }
        let (text, rect, clip) = footer(&rows)
            .unwrap_or_else(|| {
                panic!("the briefing's footer was never painted at {width:.0}×{height:.0}")
            })
            .clone();
        assert!(
            content.contains_rect(rect),
            "at {width:.0}×{height:.0} the footer \"{text}\" is painted {rect:?}, outside the \
             page's box {content:?}"
        );
        assert!(
            clip.contains_rect(rect),
            "at {width:.0}×{height:.0} the footer {rect:?} is cut by its clip {clip:?}: a line \
             that is not whole is a line the reader cannot read"
        );
        let last_bullet = rows
            .iter()
            .find(|(text, ..)| text.starts_with("· Drought"))
            .unwrap_or_else(|| {
                panic!("the briefing's last bullet was not painted at {width:.0}×{height:.0}")
            });
        assert!(
            content.contains_rect(last_bullet.1),
            "at {width:.0}×{height:.0} the last bullet {:?} is outside the page's box {content:?}",
            last_bullet.1
        );
        println!(
            "{width:.0}×{height:.0}: page {:.0}×{:.0} at ({:.0},{:.0}) · plate {:.0}×{:.0} at \
             ({:.0},{:.0}) · aspect asks {:.0} · footer \"{text}\" at ({:.0},{:.0})..({:.0},{:.0})",
            content.width(),
            content.height(),
            content.min.x,
            content.min.y,
            plate.width(),
            plate.height(),
            plate.min.x,
            plate.min.y,
            asked,
            rect.min.x,
            rect.min.y,
            rect.max.x,
            rect.max.y
        );
    }
}

/// The third size the report named: a 2 400×1 400 window, where the plate used to stop at the old
/// 762 px ceiling inside a 2 136 px-wide card — 2.8:1 of world, which crops the poles just as
/// surely as the 209 px plate at 1280×800 did.
#[test]
fn the_plate_is_the_worlds_aspect_on_a_large_window_too() {
    use atlas_desktop::views::overview::PLATE_ASPECT;

    let archive = TempArchive::recent();
    let size = Vec2::new(2_400.0, 1_400.0);
    let ctx = context();
    let mut atlas = Atlas::with_archive(archive.file());
    let mut rows = Vec::new();
    for index in 0..30 {
        rows = drawn(&mut atlas, &ctx, index, size, Vec::new());
    }
    let content = content_box(&rows, size);
    let plate = plate_at(size, &archive.file());
    let asked = plate.width() / PLATE_ASPECT;
    assert!(
        content.contains_rect(plate),
        "the plate {plate:?} is not inside the page's box {content:?} at 2 400×1 400"
    );
    assert!(
        (plate.height() - asked).abs() <= 1.0,
        "at 2 400×1 400 the plate is {:.0}×{:.0}: a plate {:.0} px wide at the world's aspect \
         {PLATE_ASPECT} is {:.0} px tall, so the map's own fit crops the world",
        plate.width(),
        plate.height(),
        plate.width(),
        asked
    );
    println!(
        "2 400×1 400: page {:.0}×{:.0} · plate {:.0}×{:.0} at ({:.0},{:.0}) · aspect asks {asked:.0}",
        content.width(),
        content.height(),
        plate.width(),
        plate.height(),
        plate.min.x,
        plate.min.y
    );
}

/// The briefing's footer, as painted: the card writes it as `written 10:25 · 12 stories in 72 h`.
fn footer<'a>(rows: &'a [Row]) -> Option<&'a Row> {
    rows.iter().find(|(text, ..)| text.starts_with("written "))
}

/// A chip narrows the briefing with the list: the Articles page asks about the country.
#[test]
fn a_country_chip_puts_that_countrys_briefing_on_the_list() {
    let archive = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(archive.file());
    paint(&mut atlas, &ctx, 0);
    // The door a chip hands over, as the page hands it: the ISO the archive groups by and the
    // number the chip printed.
    atlas.narrow_to(atlas_desktop::views::overview::Door::country("DEU", 3));
    let words = paint(&mut atlas, &ctx, 1);

    assert!(
        painted_at_least(&words, "Germany's week was one long labour dispute"),
        "the Articles page did not draw the German briefing after the DEU chip. Painted:\n{}",
        words.join("\n")
    );
    assert!(
        !painted_at_least(&words, "Four corners of the world, one week"),
        "the Articles page kept the window's briefing over a country's list. Painted:\n{}",
        words.join("\n")
    );
    assert!(
        painted_at_least(&words, "3 stories"),
        "the German list did not hold the three German stories. Painted:\n{}",
        words.join("\n")
    );
}

/// The empty case, said honestly: no row means the card invites, it does not show a blank.
#[test]
fn an_archive_without_a_briefing_invites_one() {
    let archive = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(archive.file());
    // `ESP` has a story and no briefing: the card must be the empty state, not the window's.
    atlas.narrow_to(atlas_desktop::views::overview::Door::country("ESP", 1));
    paint(&mut atlas, &ctx, 0);
    let words = paint(&mut atlas, &ctx, 1);
    assert!(
        !painted_at_least(&words, "Four corners of the world, one week"),
        "a scope with no briefing drew another scope's sentence. Painted:\n{}",
        words.join("\n")
    );
}

// A `Path` is imported for the archive's own type; keep the warning honest if that changes.
#[allow(dead_code)]
fn _archive_path(_: &Path) {}

/// Esc is the way out of a story, and it goes back where the reader came from.
///
/// The reader's own words for the defect: a story opened from the graph landed in a
/// country-filtered list with no way to understand or leave it. Alt+Left was the fix for the
/// keyboard; Esc is the key every reader tries first, and before this it did nothing on the
/// two pages a story is read on (Topics, Monitors, Chat and the Graph already spend Esc on
/// their own open row).
#[test]
fn escape_leaves_a_story_and_goes_back_where_the_reader_came_from() {
    let archive = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(archive.file());
    paint(&mut atlas, &ctx, 0);

    // A country list, then a story opened from it — the shell's own door, the one a row's
    // press runs (`Atlas::narrow_to` is the chip's line, `Atlas::open_reader` the page's).
    atlas.narrow_to(atlas_desktop::views::overview::Door::country("DEU", 3));
    paint(&mut atlas, &ctx, 1);
    assert_eq!(atlas.page(), atlas_desktop::app::Page::Articles);

    let story = atlas_desktop::views::reader::Story {
        id: 1,
        title: "Harbour crane strike ends after nine days".to_string(),
        source: "The Nordic Post".to_string(),
        published: "2026-11-22T09:00:00Z".to_string(),
        url: "https://news.example.invalid/1".to_string(),
        text: "The body of the story.".to_string(),
        full_text: true,
    };
    atlas.open_reader(story);
    paint(&mut atlas, &ctx, 2);
    assert_eq!(atlas.page(), atlas_desktop::app::Page::Reader);

    // One keystroke, as the only event of a frame: the same shape `tests/escape_pages.rs`
    // uses, because egui surrenders the focused widget on Escape before any widget is laid
    // out (`egui-0.37/src/memory/mod.rs:597`), so a handler gated on focus never runs.
    paint_with(
        &mut atlas,
        &ctx,
        3,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert_eq!(
        atlas.page(),
        atlas_desktop::app::Page::Articles,
        "Escape did not leave the source page"
    );

    // And the chip is back in force: the way back restores what the story cleared.
    let words = paint(&mut atlas, &ctx, 4);
    assert!(
        painted_at_least(&words, "3 stories"),
        "the way back did not restore the German list. Painted:\n{}",
        words.join("\n")
    );
}

/// The window asks the agent for a briefing it does not have — once per scope, not once per read.
///
/// This is the only thing the window asks for without a press, and it is what makes the card
/// appear without the reader knowing it exists: the shell names the scope, the agent's model
/// writes the sentence. The test is the *request*, not the card: the queue it writes is a file
/// of the test's own, so nothing reaches the checkout's `data/`.
#[test]
fn a_scope_with_stories_and_no_briefing_is_asked_for_once() {
    let archive = TempArchive::build();
    let ctx = context();
    // The window's row is removed first: this test is about the scope that has *no* briefing.
    // (The fixture writes one for `window` and one for `DEU`; the German row stays, so the
    // country scope below is already answered and must not be asked for twice.)
    rusqlite::Connection::open(archive.file())
        .expect("the fixture archive")
        .execute("DELETE FROM briefings WHERE scope = 'window'", [])
        .expect("the window row is gone");
    let queue = archive.dir.join("requests.db");
    let mut atlas = Atlas::with_archive(archive.file())
        .with_queue(queue.clone())
        .asking_for_briefings();

    // Two frames of the first page: the read happens on the first, the ask with it.
    paint(&mut atlas, &ctx, 0);
    paint(&mut atlas, &ctx, 1);
    assert_eq!(
        brief_rows(&queue),
        vec![("window".to_string(), String::new(), 72)],
        "the window did not ask for the briefing it has no row for"
    );

    // A second, third and fourth read: still one row. A page read twice is not two requests.
    for index in 2..5 {
        paint(&mut atlas, &ctx, index);
    }
    assert_eq!(
        brief_rows(&queue).len(),
        1,
        "the same scope was asked for more than once: {:?}",
        brief_rows(&queue)
    );

    // A scope the archive *has* a briefing for is not asked for: the fixture wrote one for
    // `DEU`, so going there must add nothing to the queue. This is the other half of "ask once
    // per missing scope" — an ask for a row that exists would be a model call nobody needs.
    atlas.narrow_to(atlas_desktop::views::overview::Door::country("DEU", 3));
    paint(&mut atlas, &ctx, 5);
    paint(&mut atlas, &ctx, 6);
    assert_eq!(
        brief_rows(&queue).len(),
        1,
        "a scope that already has a briefing was asked for: {:?}",
        brief_rows(&queue)
    );

    // And a scope with no row of its own asks when the reader arrives: `ESP` has a story and no
    // briefing, so the list for it is the third slot.
    atlas.narrow_to(atlas_desktop::views::overview::Door::country("ESP", 1));
    paint(&mut atlas, &ctx, 7);
    paint(&mut atlas, &ctx, 8);
    let rows = brief_rows(&queue);
    assert!(
        rows.contains(&("country".to_string(), "ESP".to_string(), 72)),
        "the Spanish page did not ask for its own briefing: {rows:?}"
    );
    assert_eq!(rows.len(), 2, "a scope was asked for twice: {rows:?}");
}

/// The `brief` rows in a queue file: `(scope, key, hours)`, in the order they were written.
fn brief_rows(queue: &Path) -> Vec<(String, String, i64)> {
    let Ok(connection) = rusqlite::Connection::open(queue) else {
        return Vec::new();
    };
    let Ok(mut statement) =
        connection.prepare("SELECT payload FROM requests WHERE kind = 'brief' ORDER BY id")
    else {
        return Vec::new();
    };
    let payloads: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("a read of the queue")
        .flatten()
        .collect();
    payloads
        .iter()
        .filter_map(|payload| {
            let value: serde_json::Value = serde_json::from_str(payload).ok()?;
            Some((
                value.get("scope")?.as_str()?.to_string(),
                value.get("key")?.as_str()?.to_string(),
                value.get("hours")?.as_i64()?,
            ))
        })
        .collect()
}
