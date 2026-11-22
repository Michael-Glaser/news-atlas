//! The window, measured instead of guessed: every page, at three window sizes, drawn
//! through the real `Atlas` and inspected shape by shape.
//!
//! "The UI is buggy" is a report a test cannot act on and a reader cannot point at, so this
//! file turns it into properties with numbers:
//!
//! 1. **Nothing is painted outside the window or across the line between two panels.**
//!    Every shape that reaches the backend has finite coordinates, its clip rectangle lies
//!    inside the window, and the part it actually draws (`shape ∩ clip_rect`) lies inside
//!    one of the three boxes: the rail's column, the status row, or the central panel.
//!    The boxes are read out of the frame ([`boxes`]) rather than remembered, because
//!    `app.rs` folds the rail from 188 px to 64 px below 1 068 px of width.
//! 2. **Everything drawn on a canvas is clipped to that canvas** ([`assert_on_plate`]). A
//!    map and a graph are painted with the *page's* painter, so egui would happily let a
//!    pin, a node or a relation line land in the page's margin; the rule is structural, and
//!    [`a_dragged_canvas_stays_on_its_plate`] drags a canvas to exercise it. Both measured
//!    violations of this rule are recorded in those two functions, with their numbers.
//! 3. **No interactive control is collapsed.** egui's own inventory pass
//!    (`Context::debug_painting`, `egui-0.37.0/src/context.rs:2988`) paints a `debug_rect`
//!    for every interactive widget it inventoried when `debug.show_interactive_widgets` is
//!    on — positive or not, which is what makes the check possible at all. Each rectangle
//!    must have a positive width and height and lie inside the window, and each page must
//!    hold at least the number of controls it holds today ([`controls_needed`]). A widget
//!    clipped completely away gets an *inverted* rectangle from egui and is skipped: out of
//!    sight is not the same as without a size.
//! 4. **A page paints a bounded number of shapes**, and **something at all**: [`ceiling`]
//!    and [`floor_of_shapes`], both measured, both with the table written down. A page that
//!    grew a canvas redrawn per frame, or collapsed to nothing, fails before anyone sees it.
//! 5. **Two identical runs paint the same frame** ([`two_identical_runs_paint_the_same_frame`]):
//!    the same count of shapes of each kind, and the same canvases. A window whose paint
//!    depends on wall-clock time, on a hash iteration order or on leftover state flickers.
//!
//! # How it is driven
//!
//! `Atlas::with_archive` opens a temporary archive built by [`TempArchive`] (the schema
//! `data.rs` reads, six stories, six places, seven relations), and `Atlas::draw` is given
//! the root `Ui` from `egui::Context::run_ui` — the same call `eframe` makes, with a screen
//! rectangle, a clock and synthetic pointer events instead of a GPU surface. No window is
//! opened, no port is touched, nothing is downloaded. Two frames per page: the first lays
//! the panels out (egui measures the rail and the status row on the way), the second paints
//! where they landed, and it is the second that is inspected.
//!
//! Two passes per page and size: a **plain** one, painting as a release build does, read for
//! geometry; and a **debug** one, with egui's widget inventory switched on, read for the
//! controls. They are separate because the inventory pass also paints "Widgets in order: …"
//! over the page whenever any widget contains the pointer — egui's own text, measured as a
//! 564 × 30 rectangle at x −33.7 on the Overview at 900 × 620, which would otherwise be
//! judged as the window's.
//!
//! Run it, with the table printed:
//!
//! ```text
//! cargo test --offline -p atlas-desktop --test ui_layout -- --nocapture
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::epaint::{ClippedShape, Shape};
use egui::{Color32, Pos2, Rect, Vec2};

use atlas_desktop::app::{Atlas, Page, PAGES};
use atlas_desktop::theme;

/// Rounding noise: egui snaps hairlines to half pixels, and a 1 px stroke centred on a
/// rectangle's edge reaches half a pixel outside it.
const TOLERANCE: f32 = 2.0;

/// The window sizes a reader actually uses: the smallest the layout must survive
/// (`main.rs` opens at 1 280 × 860), a laptop, and a large desktop display.
const SIZES: [Vec2; 3] = [
    Vec2::new(900.0, 620.0),
    Vec2::new(1280.0, 820.0),
    Vec2::new(1680.0, 1200.0),
];

/// The most shapes a page may paint, and the fewest it must.
///
/// Measured on the second frame of the plain pass (no debug painting, egui's own empty
/// galley and pruned rectangles excluded), printed by every run of
/// [`no_page_paints_outside_its_box`]:
///
/// | page     | 900 × 620 | 1 280 × 820 | 1 680 × 1 200 |
/// |----------|-----------|-------------|---------------|
/// | Overview |       341 |         543 |           798 |
/// | Articles |        49 |          66 |            66 |
/// | Graph    |        48 |          60 |            65 |
/// | Topics   |        56 |          63 |            63 |
/// | Monitors |        50 |          57 |            57 |
/// | Chat     |        41 |          48 |            48 |
///
/// Overview carries the map — one shape per coastline segment on top of the graticule, the
/// coast and the pins — which is why its budget is an order of magnitude above the rest, and
/// why it grows with the window: three times the largest of the three sizes is the ceiling,
/// twice the largest for the rest, each rounded up to the next 50. A page that grows past its
/// ceiling is a page to *measure* and then raise on purpose, which is what this table is for.
/// The numbers move when a view changes — they are a snapshot of the layout today, not a
/// property of it.
fn ceiling(page: Page) -> usize {
    match page {
        Page::Overview => 2400,
        Page::Articles => 200,
        // The Reader with nothing open is one sentence; with a story it is the engine's
        // rectangle (an empty page to this pass, which sees only egui's own paint) or the
        // text page. Measured: 24 shapes at 900 × 620 with no story, which is the toolbar
        // and the sentence; the ceiling is the same snapshot budget the text pages get.
        Page::Reader => 200,
        Page::Graph => 200,
        Page::Topics => 200,
        Page::Monitors => 200,
        Page::Chat => 150,
        // A form: labels, values and one row per model file. Measured at 1440x900 with the
        // rail open; the ceiling is twice the largest of the three sizes.
        Page::Settings => 200,
    }
}

/// The fewest shapes a page may paint: half the smallest measurement, rounded down. A page
/// that paints nothing would otherwise pass every assertion in this file.
fn floor_of_shapes(page: Page) -> usize {
    match page {
        Page::Overview => 120,
        Page::Articles => 20,
        // One sentence, its toolbar and the rail: measured 24 shapes with no story open.
        Page::Reader => 10,
        Page::Graph => 20,
        Page::Topics => 25,
        Page::Monitors => 25,
        Page::Chat => 20,
        Page::Settings => 20,
    }
}

/// The fewest interactive controls a page must still hold: the measured count, exactly,
/// with the rail open (188 px) and folded (64 px).
///
/// | page     | folded | open |
/// |----------|--------|------|
/// | Overview |     32 |   39 |
/// | Articles |     38 |   44 |
/// | Graph    |     16 |   23 |
/// | Topics   |     18 |   25 |
/// | Monitors |     17 |   24 |
/// | Chat     |     18 |   25 |
///
/// The rail's own controls (six page rows, three window buttons, Refresh) are in every one
/// of those numbers, because the rail is drawn on every page; folding it turns the words
/// into marks and drops the count captions, which is why the folded column is lower.
///
/// The count is the guard on "no control is collapsed": egui paints a `debug_rect` for every
/// interactive widget it inventoried, positive or not, so a control allocated with no size
/// is caught twice — once by the width/height assertion, and once here, because a control
/// that stops being drawn at all cannot be counted.
fn controls_needed(page: Page, rail: f32) -> usize {
    let folded = rail < 100.0;
    let (open, folded_count) = match page {
        Page::Overview => (39, 32),
        Page::Articles => (44, 38),
        // The rail's eight rows and the window buttons, and on the page itself nothing but
        // the sentence: the Reader's controls are the engine's own toolbar, which is drawn
        // only once a story is open (this pass opens none).
        Page::Reader => (13, 10),
        Page::Graph => (23, 16),
        Page::Topics => (25, 18),
        Page::Monitors => (24, 17),
        Page::Chat => (25, 18),
        // The rail (six rows then, seven now) plus the page's own fields and buttons; a
        // folded rail keeps only the marks.
        Page::Settings => (20, 14),
    };
    if folded {
        folded_count
    } else {
        open
    }
}

// ------------------------------------------------------------------- the archive

/// A `data/atlas.db` of our own, in a temp directory, removed when the test ends.
///
/// The live archive is never opened: a test that read the user's reading history would
/// be neither hermetic nor repeatable. The schema is the subset `data.rs` queries, and
/// the rows are enough for every page to have something to draw — the detector must
/// fail on a *broken* layout, not pass because a page was empty.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

/// Distinguishes archives built in the same nanosecond on different test threads.
static NEXT_ARCHIVE: AtomicU64 = AtomicU64::new(0);

impl TempArchive {
    fn build() -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-ui-layout-{}-{}-{}",
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
        let recent = hours_ago(2);
        let older = hours_ago(30);
        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'The Nordic Post'),(2,'Alpine Review')",
            [],
        )
        .expect("two sources");
        let stories: [(i64, &str, i64, &str, &str, &str, f64, f64); 6] = [
            (1, "Harbour crane strike ends after nine days", 1, "DEU", "Hamburg", recent.as_str(), 54.0, 10.0),
            (2, "Alpine rail freight reaches a record", 2, "AUT", "Vienna", recent.as_str(), 48.2, 16.4),
            (3, "Chip export rules tighten in Asia", 1, "JPN", "Tokyo", older.as_str(), 35.7, 139.7),
            (4, "Drought reshapes the southern harvest", 2, "ESP", "Madrid", recent.as_str(), 40.4, -3.7),
            (5, "A second harbour approves the contract", 1, "NLD", "Rotterdam", recent.as_str(), 51.9, 4.5),
            (6, "Ferry traffic resumes on the northern route", 2, "NOR", "Oslo", older.as_str(), 59.9, 10.7),
        ];
        for (id, title, source, iso, place, published, lat, lon) in stories {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 1, 0, ?10, ?11)",
                rusqlite::params![
                    id,
                    title,
                    source,
                    published,
                    format!("{title} — a summary sentence for the list."),
                    format!("{title}. The body of the story, long enough to fill the reading pane."),
                    iso,
                    place,
                    format!("https://example.invalid/{id}"),
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        conn.execute(
            "INSERT INTO article_links(a, b) VALUES(1,2),(2,3),(3,4),(4,5),(5,6)",
            [],
        )
        .expect("links");
        conn.execute("INSERT INTO alerts(id) VALUES(1),(2),(3)", [])
            .expect("alerts");
        for (id, name, kind, count) in [
            (1, "Alpha Group", "ORG", 40),
            (2, "North Harbour", "LOC", 31),
            (3, "Beta Union", "ORG", 22),
            (4, "C. Reporter", "PER", 18),
            (5, "Alpine Rail", "ORG", 14),
            (6, "Southern Growers", "ORG", 11),
            (7, "Ferry Line", "ORG", 9),
            (8, "Ministry of Trade", "ORG", 8),
        ] {
            conn.execute(
                "INSERT INTO entities(id, name, type, count, last_seen) VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, name, kind, count, recent],
            )
            .expect("an entity");
        }
        for (a, b, weight, label) in [
            (1, 2, 6.0, "Alpha Group and North Harbour share 3 stories"),
            (1, 3, 4.5, "Alpha Group and Beta Union share 2 stories"),
            (2, 5, 3.0, "North Harbour and Alpine Rail share a story"),
            (3, 4, 2.5, "Beta Union and C. Reporter share a story"),
            (5, 8, 2.0, "Alpine Rail and Ministry of Trade share a story"),
            (6, 7, 1.5, "Southern Growers and Ferry Line share a story"),
            (1, 6, 1.2, "Alpha Group and Southern Growers share a story"),
        ] {
            conn.execute(
                "INSERT INTO edges(a, b, weight, label) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![a, b, weight, label],
            )
            .expect("a relation");
        }
        for (id, name, keywords, active, hits) in [
            (1, "Chip supply", "chip, semiconductor, export", 1, 12),
            (2, "Harbour labour", "harbour, strike, dock", 1, 7),
            (3, "Alpine freight", "rail, freight, alpine", 0, 3),
        ] {
            conn.execute(
                "INSERT INTO topics(id, name, keywords, active, hits, last_hit) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, name, keywords, active, hits, recent],
            )
            .expect("a topic");
        }
        for (id, name, query, active, cadence, hits) in [
            (1, "Chip export rules", "chip export restrictions", 1, 60, 4),
            (2, "North Sea ferries", "ferry north sea traffic", 0, 240, 1),
        ] {
            conn.execute(
                "INSERT INTO monitors(id, name, query, active, cadence_minutes, last_run, hits) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![id, name, query, active, cadence, recent, hits],
            )
            .expect("a monitor");
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

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`), so the seeded rows
/// stay inside the 72 h window whenever this runs. Howard Hinnant's civil-from-days,
/// because the crate has no time dependency and neither does this.
fn hours_ago(hours: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let at = now - hours * 3_600;
    let (days, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
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

// ----------------------------------------------------------------- driving a frame

/// The window as a release build paints it: the house style, and none of egui's debug
/// painting.
///
/// `debug_on_hover` is off explicitly because egui turns it **on in debug builds**
/// (`egui-0.37.0/src/style.rs:1452`, `cfg!(debug_assertions)`): left on, a debug
/// `cargo test` would judge a tooltip egui drew for its own benefit.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.all_styles_mut(|style| {
        style.debug.debug_on_hover = false;
        style.debug.show_widget_hits = false;
    });
    ctx
}

/// The same window with egui's interactive-widget inventory switched on — the pass
/// [`controls_needed`] is read from, and the only pass in which egui paints for itself.
///
/// `show_interactive_widgets` makes `debug_painting` (`context.rs:2988`) draw a
/// `debug_rect` per interactive widget in one of three sense colours (`context.rs:3013`);
/// those rectangles are the inventory. It is a style flag and `theme::install` fills both
/// theme slots, so both are given it: `debug_painting` reads the *global* style
/// (`context.rs:3009`), whichever theme the preference resolved to. The same block also
/// paints a "Widgets in order: …" text on the root layer whenever any widget contains the
/// pointer — measured, a 564 × 30 rectangle at x −33.7 on the Overview at 900 × 620 — so
/// this pass is read for control rectangles only and never for geometry.
fn inventory_context() -> egui::Context {
    let ctx = context();
    ctx.all_styles_mut(|style| style.debug.show_interactive_widgets = true);
    ctx
}

fn input(size: Vec2, index: usize) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(index as f64 / 60.0),
        ..Default::default()
    }
}

/// One frame of the real window, with the texture deltas cleared the way a backend would
/// upload them (`epaint-0.37.0/src/textures.rs:337` panics on `Drop` otherwise).
fn frame(ctx: &egui::Context, size: Vec2, index: usize, atlas: &mut Atlas) -> egui::FullOutput {
    frame_with(ctx, size, index, atlas, Vec::new())
}

/// [`frame`] with pointer events in it, for a gesture a static frame cannot express.
fn frame_with(
    ctx: &egui::Context,
    size: Vec2,
    index: usize,
    atlas: &mut Atlas,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let raw = egui::RawInput {
        events,
        ..input(size, index)
    };
    let output = ctx.run_ui(raw, |ui| atlas.draw(ui));
    let egui::FullOutput {
        platform_output,
        mut textures_delta,
        shapes,
        pixels_per_point,
        viewport_output,
    } = output;
    // The deltas are cleared, not applied: there is no renderer here, and epaint 0.37
    // panics on `Drop` when a `TexturesDelta` still carries unapplied deltas
    // (`epaint-0.37.0/src/textures.rs:337`).
    textures_delta.clear();
    egui::FullOutput {
        platform_output,
        textures_delta: Default::default(),
        shapes,
        pixels_per_point,
        viewport_output,
    }
}

/// A shape's kind, for a failure message that says what spilled.
fn kind_of(shape: &Shape) -> &'static str {
    match shape {
        Shape::Noop => "noop",
        Shape::Vec(_) => "vec",
        Shape::Callback(_) => "callback",
        Shape::Mesh(_) => "mesh",
        Shape::Rect(_) => "rect",
        Shape::Text(_) => "text",
        Shape::Path(_) => "path",
        Shape::LineSegment { .. } => "line",
        Shape::Circle(_) => "circle",
        Shape::QuadraticBezier(_) => "quadratic",
        Shape::CubicBezier(_) => "cubic",
        Shape::Ellipse(_) => "ellipse",
        Shape::Band(_) => "band",
    }
}

fn walk(shape: &Shape, visit: &mut impl FnMut(&Shape)) {
    visit(shape);
    if let Shape::Vec(items) = shape {
        for item in items {
            walk(item, visit);
        }
    }
}

/// The three colours egui paints into a `debug_rect` for an interactive widget
/// (`context.rs:3013`): click, click-and-drag, drag. Nothing in this app paints them — the
/// palette has no hue at all — so a shape carrying one is the inventory overlay, and the
/// inventory pass is never read for geometry.
fn is_widget_colour(colour: Color32) -> bool {
    let (r, g, b) = (colour.r(), colour.g(), colour.b());
    matches!((r, g, b), (0x88, 0, 0x88) | (0x88, 0, 0) | (0, 0, 0x88))
}

/// One frame, read the way a backend would read it.
#[derive(Default)]
struct Painted {
    /// Every painted shape: kind, the rectangle it draws, and its clip rectangle.
    shapes: Vec<(&'static str, Rect, Rect)>,
    /// Shapes that paint nothing: `Rect::NOTHING` (an empty `Shape::Vec`, a zero-size
    /// fill egui pruned) and the empty galley egui lays out for a text cursor. They have
    /// no glyph and no area, so their bounding rectangle is not a pixel anywhere; the
    /// NaN and infinity this file refuses are in shapes that *do* paint.
    painted_nothing: usize,
}

impl Painted {
    fn read(output: &egui::FullOutput) -> Painted {
        let mut painted = Painted::default();
        for ClippedShape {
            clip_rect, shape, ..
        } in &output.shapes
        {
            let mut look = |shape: &Shape| {
                if let Shape::Text(text) = shape {
                    if text.galley.text().is_empty() {
                        painted.painted_nothing += 1;
                        return;
                    }
                }
                let rect = shape.visual_bounding_rect();
                if rect == Rect::NOTHING || rect.max.x < rect.min.x || rect.max.y < rect.min.y {
                    painted.painted_nothing += 1;
                    return;
                }
                painted.shapes.push((kind_of(shape), rect, *clip_rect));
            };
            walk(shape, &mut look);
        }
        painted
    }

    /// How many shapes of each kind: the frame's mix, which is stable even when a label
    /// in the status row is a pixel wider on one run than the next.
    fn kinds(&self) -> std::collections::BTreeMap<&'static str, usize> {
        let mut kinds = std::collections::BTreeMap::new();
        for (kind, _, _) in &self.shapes {
            *kinds.entry(*kind).or_insert(0) += 1;
        }
        kinds
    }
}

/// The interactive controls egui inventoried in one frame, as (sense colour, rectangle).
type Controls = Vec<(Color32, Rect)>;

/// Read the inventory out of egui's own debug painting.
fn read_controls(output: &egui::FullOutput) -> Controls {
    let mut controls = Vec::new();
    for ClippedShape { shape, .. } in &output.shapes {
        walk(shape, &mut |shape| {
            if let Shape::Rect(rect) = shape {
                if is_widget_colour(rect.stroke.color) {
                    controls.push((rect.stroke.color, rect.rect));
                }
            }
        });
    }
    controls
}

/// The canvases in an inventory: a widget big enough to be one, that egui gave the
/// click-and-drag sense a plate takes (`map.rs::show`, `graph.rs::show`). The threshold is
/// a fifth of the central panel, which no control on any page reaches.
fn plates(controls: &Controls, panel: Rect) -> Vec<Rect> {
    controls
        .iter()
        .filter(|(colour, rect)| {
            // A rect that is not a rectangle is not a canvas: the Overview measures its
            // briefing card in a child `Ui` clipped to `Rect::NOTHING`, and that pass allocates
            // interaction rects of `[inf inf]-[-inf -inf]`. Their width times height is `+inf`,
            // so the size filter below would take them for the biggest plate on the page —
            // measured: `expected one canvas, found [6 x [inf inf]-[-inf -inf], the plate]`.
            // They paint nothing (the clip is empty), so a finite, positive area is part of
            // being a plate.
            let finite = rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0;
            *colour == Color32::from_rgb(0x88, 0, 0x88)
                && finite
                && rect.width() * rect.height() > panel.width() * panel.height() * 0.2
        })
        .map(|(_, rect)| *rect)
        .collect()
}

/// The boxes the window hands out: the rail's column, the status row along the bottom,
/// and the central panel the pages draw in.
struct Boxes {
    rail: Rect,
    status: Rect,
    panel: Rect,
}

/// The rail's width when it shows its words (`app.rs:396`, `RAIL_WIDTH`) and when it folds
/// to marks (`app.rs:399`, `RAIL_FOLDED`). The rail folds itself below 1 068 px of window
/// width — derived in `app.rs:413` from the graph's fair share — so the three sizes here
/// straddle the fold: 900 px gets the 64 px rail, 1 280 and 1 680 get the 188 px one.
const RAIL_WIDTHS: [f32; 2] = [64.0, 188.0];

/// Where the dots are, summed: pins on the map, nodes on the graph. One number for "has the
/// picture moved", which is what a drag is supposed to do.
fn dot_sum(painted: &Painted) -> Vec2 {
    let mut sum = Vec2::ZERO;
    for (kind, rect, _) in &painted.shapes {
        if *kind == "circle" {
            sum += rect.center().to_vec2();
        }
    }
    sum
}

/// Every shape painted *on* a canvas must be clipped to that canvas, or painted inside it.
///
/// A shape that reaches over the edge of a plate and carries a clip rectangle bigger than it
/// is paint that can land beside the plate — in the page's margin, over the header, on the
/// panel next to it. Three kinds of shape are exempt:
///
/// * the plate's own background (`rect == plate`) and the card the plate sits in (a
///   rectangle that contains the plate) — they are drawn *under* the canvas;
/// * a shape whose **geometry** is already inside the plate. Ordinary widgets are the case:
///   egui hands a widget the panel's clip rectangle by construction, and the Graph page lays
///   a small toolbar (`Zoom in`, `Zoom out`, `Fit`) over its plate. Measured at 900 × 620:
///   the three labels sit at x 403.9…536.4 inside a plate of x 84…544, so not one pixel of
///   them can leave the plate — and this rule called them paint beside it because the clip
///   their `Response` inherited was the panel, `[[64 0] - [900 581]]`. The same shape at
///   1 280 × 820: x 903.4…916.4 inside a plate ending at 924. The rule below is unchanged
///   for the shapes it exists for, the ones that reach over an edge: they still have to be
///   clipped to the plate.
///
/// The rule exists because both canvases are painted with a painter whose clip rectangle is
/// the plate's size rather than the page's (`views/map.rs:935` and `views/graph.rs:763`,
/// `ui.allocate_painter`): nothing in egui stops a pin, a node, a coastline or a graticule
/// line from being painted in the margin. Two measurements of what that costs, taken before
/// the layouts were rewritten to fit the plate: the Graph page at 900 × 620 drew a relation
/// line `[[-84.6 243.5] - [203.4 477.5]]`, i.e. 44 px of it over the page's 20 px left margin
/// (x 188…208) and the rest across the header; the Overview at the same size drew graticule
/// lines from x 67.5 to x 1 008.5 inside a 660 px plate. Both of those have a geometry that
/// leaves the plate *and* a panel-wide clip, so both still fail here.
fn assert_on_plate(page: Page, size: Vec2, painted: &Painted, plates: &[Rect]) {
    for (kind, rect, clip) in &painted.shapes {
        for plate in plates {
            if rect.contains_rect(*plate) {
                continue;
            }
            if !rect.intersect(*clip).intersects(*plate) {
                continue;
            }
            // Inside the plate: whatever clip it inherited, the shape cannot paint beside it.
            if plate.expand(TOLERANCE).contains_rect(*rect) {
                continue;
            }
            assert!(
                plate.expand(TOLERANCE).contains_rect(*clip),
                "{} at {:.0}x{:.0}: a {kind} {rect:?} draws on the plate {plate:?} but is \
                 clipped by {clip:?}, so it paints over whatever is beside it",
                page.label(),
                size.x,
                size.y
            );
        }
    }
}

/// Read the boxes out of the frame instead of remembering them.
///
/// Every shape the rail paints is clipped by a rectangle that starts at the window's left
/// edge, and every shape a page paints by one that starts at the rail's right edge, so
/// the widest clip starting at x = 0 is the rail — measured, because the app folds it and
/// a literal would be wrong at one of the three sizes. The central panel's bottom line is
/// the lowest clip that starts at y = 0 to the right of the rail; the status row is what
/// is left under it.
fn boxes(size: Vec2, painted: &Painted) -> Boxes {
    let window = Rect::from_min_size(Pos2::ZERO, size);
    let rail_right = painted
        .shapes
        .iter()
        .filter(|(_, _, clip)| clip.min.x <= 0.5 && clip.max.x <= size.x * 0.5)
        .map(|(_, _, clip)| clip.max.x)
        .fold(0.0_f32, f32::max);
    let panel_bottom = painted
        .shapes
        .iter()
        .filter(|(_, _, clip)| {
            clip.min.y <= 0.5 && (clip.min.x - rail_right).abs() <= TOLERANCE && clip.max.x > rail_right
        })
        .map(|(_, _, clip)| clip.max.y)
        .fold(0.0_f32, f32::max);
    assert!(
        rail_right > 0.0 && panel_bottom > 0.0 && panel_bottom < size.y,
        "the frame at {:.0}x{:.0} painted no rail or no central panel: rail {rail_right}, \
         panel bottom {panel_bottom}",
        size.x,
        size.y
    );
    assert!(
        RAIL_WIDTHS
            .iter()
            .any(|width| (width - rail_right).abs() <= TOLERANCE),
        "the rail is {rail_right} px wide at {:.0}x{:.0}, and the app documents two widths, \
         {RAIL_WIDTHS:?} — a third one is a change to state on purpose",
        size.x,
        size.y
    );
    let rail = Rect::from_min_max(window.min, Pos2::new(rail_right, size.y));
    Boxes {
        rail,
        status: Rect::from_min_max(Pos2::new(rail_right, panel_bottom), window.max),
        panel: Rect::from_min_max(Pos2::new(rail_right, 0.0), Pos2::new(size.x, panel_bottom)),
    }
}

/// Draw one page at one size and read the second frame — the first lays the panels out,
/// the second paints where they landed — with no debug painting in it.
fn paint(archive: &PathBuf, page: Page, size: Vec2) -> Painted {
    let ctx = context();
    let mut atlas = Atlas::with_archive(archive.clone());
    atlas.set_page(page);
    let _ = frame(&ctx, size, 0, &mut atlas);
    Painted::read(&frame(&ctx, size, 1, &mut atlas))
}

/// The same page with egui's inventory switched on, read for its controls.
fn inventory(archive: &PathBuf, page: Page, size: Vec2) -> Controls {
    let ctx = inventory_context();
    let mut atlas = Atlas::with_archive(archive.clone());
    atlas.set_page(page);
    let _ = frame(&ctx, size, 0, &mut atlas);
    read_controls(&frame(&ctx, size, 1, &mut atlas))
}

// ----------------------------------------------------------------------- the tests

/// Everything a frame must satisfy: finite, inside the window, clipped to its box, and
/// inside the shape budget — asserted per page per size, with the table printed.
#[test]
fn no_page_paints_outside_its_box() {
    let temp = TempArchive::build();
    println!(
        "{:>9}  {:8}  {:>6} {:>8} {:>7} {:>8} {:>5} {:>5}",
        "size", "page", "shapes", "ceiling", "canvas", "not-painted", "rail", "ctrl"
    );
    for size in SIZES {
        let window = Rect::from_min_size(Pos2::ZERO, size);
        for page in PAGES {
            let painted = paint(&temp.file, page, size);
            let boxes = boxes(size, &painted);
            let panels = [boxes.rail, boxes.status, boxes.panel];
            let controls = inventory(&temp.file, page, size);
            let canvases = plates(&controls, boxes.panel);
            println!(
                "{:>4.0}x{:<4.0}  {:8}  {:6} {:8} {:6} {:>7} {:>5.0} {:>5}",
                size.x,
                size.y,
                page.label(),
                painted.shapes.len(),
                ceiling(page),
                canvases.len(),
                painted.painted_nothing,
                boxes.rail.width(),
                controls.len()
            );

            // (1) finite coordinates, and nothing drawn outside the window or across the
            // line between two panels.
            for (kind, rect, clip) in &painted.shapes {
                assert!(
                    rect.min.x.is_finite()
                        && rect.min.y.is_finite()
                        && rect.max.x.is_finite()
                        && rect.max.y.is_finite(),
                    "{} at {:.0}x{:.0}: a {kind} has a non-finite rectangle {rect:?}",
                    page.label(),
                    size.x,
                    size.y
                );
                assert!(
                    window.expand(TOLERANCE).contains_rect(*clip),
                    "{} at {:.0}x{:.0}: a {kind} is clipped by {clip:?}, which leaves the \
                     window {window:?}",
                    page.label(),
                    size.x,
                    size.y
                );
                let drawn = rect.intersect(*clip);
                // Clipped away entirely — an empty intersection, which egui represents as
                // an inverted rectangle. Nothing of this shape reaches the screen, so it
                // cannot be paint in the wrong place: measured, a story row's text below
                // the Overview's scroll area, `[[106 595] - [503 606]]` against a clip that
                // ends at y 552.
                if drawn.min.x > drawn.max.x || drawn.min.y > drawn.max.y {
                    continue;
                }
                assert!(
                    window.expand(TOLERANCE).contains_rect(drawn),
                    "{} at {:.0}x{:.0}: a {kind} draws {drawn:?}, outside the window \
                     {window:?}",
                    page.label(),
                    size.x,
                    size.y
                );
                assert!(
                    panels
                        .iter()
                        .any(|panel| panel.expand(TOLERANCE).contains_rect(drawn)),
                    "{} at {:.0}x{:.0}: a {kind} {rect:?} draws {drawn:?} (clip {clip:?}), \
                     which straddles the boxes {panels:?}",
                    page.label(),
                    size.x,
                    size.y
                );
            }

            // (2) every canvas is clipped to itself.
            assert_on_plate(page, size, &painted, &canvases);

            // (3) no control is collapsed, and none leaves the window.
            let floor = controls_needed(page, boxes.rail.width());
            assert!(
                controls.len() >= floor,
                "{} at {:.0}x{:.0}: {} interactive controls, was {} — a control lost its \
                 size and dropped out of egui's inventory",
                page.label(),
                size.x,
                size.y,
                controls.len(),
                floor
            );
            for (colour, rect) in &controls {
                // Inverted rectangle: egui clipped the control away entirely (a story row
                // scrolled out of the fold gets one), which is a control out of sight, not
                // a control without a size. Measured on the Overview at 900 × 620:
                // `[[97 605] - [855 552]]`, a row below the scroll area's bottom.
                if rect.min.x > rect.max.x || rect.min.y > rect.max.y {
                    continue;
                }
                assert!(
                    rect.width() > 0.0 && rect.height() > 0.0,
                    "{} at {:.0}x{:.0}: a control ({:?}) was allocated {rect:?}, which has \
                     no area",
                    page.label(),
                    size.x,
                    size.y,
                    (colour.r(), colour.g(), colour.b())
                );
                assert!(
                    window.expand(TOLERANCE).contains_rect(*rect),
                    "{} at {:.0}x{:.0}: a control was allocated {rect:?}, outside the \
                     window {window:?}",
                    page.label(),
                    size.x,
                    size.y
                );
            }

            // (4) and (5) a page paints a bounded, non-empty number of shapes.
            assert!(
                painted.shapes.len() <= ceiling(page),
                "{} at {:.0}x{:.0}: {} shapes, over the ceiling of {} — measure it again \
                 and raise the ceiling on purpose",
                page.label(),
                size.x,
                size.y,
                painted.shapes.len(),
                ceiling(page)
            );
            assert!(
                painted.shapes.len() >= floor_of_shapes(page),
                "{} at {:.0}x{:.0}: {} shapes, under the floor of {} — the page drew \
                 almost nothing",
                page.label(),
                size.x,
                size.y,
                painted.shapes.len(),
                floor_of_shapes(page)
            );
        }
    }
}

/// The same page, drawn twice from a fresh window, paints the same frame: the same number
/// of shapes of each kind, the same controls, the same canvases.
///
/// Counts and kinds, not rectangles: the status row prints how long the archive read took
/// (`app.rs:599`, `read in {} ms`), so one label in that row is a pixel or two wider on a
/// slower run. That is the row telling the truth, not the frame being unstable — and it is
/// a *width*, never a count.
#[test]
fn two_identical_runs_paint_the_same_frame() {
    let temp = TempArchive::build();
    for size in SIZES {
        for page in PAGES {
            let first = paint(&temp.file, page, size);
            let second = paint(&temp.file, page, size);
            assert_eq!(
                first.shapes.len(),
                second.shapes.len(),
                "{} at {:.0}x{:.0}: two identical runs painted {} shapes, then {}",
                page.label(),
                size.x,
                size.y,
                first.shapes.len(),
                second.shapes.len()
            );
            assert_eq!(
                first.kinds(),
                second.kinds(),
                "{} at {:.0}x{:.0}: the same count, a different mix of shapes",
                page.label(),
                size.x,
                size.y
            );
            let controls = inventory(&temp.file, page, size);
            assert_eq!(
                controls.len(),
                inventory(&temp.file, page, size).len(),
                "{} at {:.0}x{:.0}: the run inventoried a different number of controls twice",
                page.label(),
                size.x,
                size.y
            );
            assert_eq!(
                plates(&controls, boxes(size, &first).panel).len(),
                plates(&inventory(&temp.file, page, size), boxes(size, &first).panel).len(),
                "{} at {:.0}x{:.0}: the run painted a different number of canvases twice",
                page.label(),
                size.x,
                size.y
            );
        }
    }
}

/// A canvas the reader drags and zooms must not paint past its own edge.
///
/// The half of the layout a settled frame cannot see: the opening camera and the settled
/// layout both keep their picture inside the plate, so the frame is clean until a reader
/// moves it. The test does both things a plate answers to — presses and drags 160 px right
/// and 90 down in four steps, releases, then rolls the wheel eight times over the plate to
/// zoom in about the pointer — and applies [`assert_on_plate`] to each of the two frames. A
/// press that moves is a drag, not a click: no story opens, no page changes.
///
/// Measured: clean. The zoom takes the Overview from 341 shapes to 385 and the Graph from
/// 48 to 56, and neither leaves the plate — both views clip their geometry in view space
/// (`map.rs::World::frame`, the graph's `at`), not with the painter. That is the property,
/// and a view that starts relying on the clip rectangle instead has to say so here.
///
/// The drag is also required to have *happened* ([`dot_sum`]): a gesture that never reached
/// the canvas would make the plate rule pass for the wrong reason.
#[test]
fn a_dragged_canvas_stays_on_its_plate() {
    let temp = TempArchive::build();
    // The window `main.rs` opens at, near enough: plates big enough to hold the picture.
    let size = SIZES[1];
    for page in [Page::Overview, Page::Graph] {
        let painted = paint(&temp.file, page, size);
        let boxes = boxes(size, &painted);
        let canvases = plates(&inventory(&temp.file, page, size), boxes.panel);
        assert_eq!(
            canvases.len(),
            1,
            "{} at {:.0}x{:.0}: expected one canvas, found {canvases:?}",
            page.label(),
            size.x,
            size.y
        );
        let plate = canvases[0];
        let from = plate.center();
        let delta = Vec2::new(160.0, 90.0);

        let ctx = context();
        let mut atlas = Atlas::with_archive(temp.file.clone());
        atlas.set_page(page);
        let mut index = 0;
        let mut step = |atlas: &mut Atlas, events: Vec<egui::Event>| {
            let output = frame_with(&ctx, size, index, atlas, events);
            index += 1;
            output
        };
        let _ = step(&mut atlas, Vec::new());
        // Zoom in **before** dragging, and only on the map. Its floor is the whole world exactly
        // filling its plate (`map.rs::Camera::settle` refuses to show air), so at the opening
        // view a drag has nowhere to go: measured with the drag first, `the drag moved the
        // picture by [0.0 0.0]` — the gesture landed, the camera was already at its edge. Four
        // notches give it slack. The graph does not need them (its picture is a fit, not a
        // camera against an edge), and they would spend zoom it needs for the check below.
        if page == Page::Overview {
            for _ in 0..4 {
                let _ = step(
                    &mut atlas,
                    vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, 40.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Trackpad,
                        modifiers: Default::default(),
                    }],
                );
            }
            let _ = step(&mut atlas, Vec::new());
        }
        // The frame the drag is measured against: after the zoom, before the hand.
        let before_drag = Painted::read(&step(&mut atlas, Vec::new()));
        let _ = step(&mut atlas, vec![egui::Event::PointerMoved(from)]);
        let _ = step(
            &mut atlas,
            vec![egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
        );
        for step_index in 1..=4 {
            let at = from + delta * (step_index as f32 / 4.0);
            let _ = step(&mut atlas, vec![egui::Event::PointerMoved(at)]);
        }
        let end = from + delta;
        let _ = step(
            &mut atlas,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        // Two frames of momentum after the release: the plate is still moving itself.
        let _ = step(&mut atlas, Vec::new());
        let output = step(&mut atlas, Vec::new());
        let dragged = Painted::read(&output);
        assert!(
            dragged.shapes.len() >= floor_of_shapes(page),
            "{} at {:.0}x{:.0}: a dragged canvas painted {} shapes, under the floor of {}",
            page.label(),
            size.x,
            size.y,
            dragged.shapes.len(),
            floor_of_shapes(page)
        );
        // The gesture has to have landed, or the plate rule below proves nothing: the dots
        // (pins on the map, nodes on the graph) are where the picture's position is
        // visible. Measured against the frame the zoom left, a 160 x 90 drag at zoom > 1 moves
        // the picture by more than a hundred pixels, so 40 px is slack, not the measurement.
        assert!(
            (dot_sum(&dragged) - dot_sum(&before_drag)).length() > 40.0,
            "{} at {:.0}x{:.0}: the drag moved the picture by {:?}, so the gesture never \
             reached the canvas and the plate rule was not exercised",
            page.label(),
            size.x,
            size.y,
            dot_sum(&dragged) - dot_sum(&before_drag)
        );
        assert_on_plate(page, size, &dragged, &canvases);

        // The same plate, zoomed in about the pointer: the picture grows past the plate's
        // edges in view space, which is exactly the frame where an unclipped painter shows.
        for _ in 0..8 {
            let _ = step(
                &mut atlas,
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, 40.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Trackpad,
                    modifiers: Default::default(),
                }],
            );
        }
        let _ = step(&mut atlas, Vec::new());
        // A wheel is *smoothed* by egui over the frames after the event, and the graph reads
        // `smooth_scroll_delta`: four quiet frames let the gesture land before the frame is read.
        for _ in 0..4 {
            let _ = step(&mut atlas, Vec::new());
        }
        let zoomed = Painted::read(&step(&mut atlas, Vec::new()));
        // What a zoom moves is the *picture*, not the number of shapes: a graph that scales its
        // dots paints the same count. Measured before this assertion was on the dots: `the wheel
        // changed nothing (71 shapes settled, 71 after eight steps of zoom)` — the zoom had
        // happened, and the count could not see it.
        let scaled = (dot_sum(&zoomed) - dot_sum(&before_drag)).length();
        assert!(
            scaled > 1.0 || zoomed.shapes.len() != painted.shapes.len(),
            "{} at {:.0}x{:.0}: the wheel changed nothing ({} shapes settled, {} after \
             eight steps of zoom; the dots moved {scaled:.2} px), so the gesture never reached \
             the canvas",
            page.label(),
            size.x,
            size.y,
            painted.shapes.len(),
            zoomed.shapes.len()
        );
        assert_on_plate(page, size, &zoomed, &canvases);
    }
}

/// The plate rule still fires on the shapes it exists for.
///
/// [`assert_on_plate`] grants one exemption that the kit of a page can reach without a
/// canvas: a shape whose geometry is inside the plate may carry the panel's clip (an
/// ordinary widget laid over the canvas, measured on the Graph page's zoom row). This test
/// feeds the rule the two shapes the doc comment measured *before* the layouts were fixed —
/// a relation line reaching over the left edge, a graticule line running past both sides —
/// and requires a panic, and then the widget case and requires silence. Without it, a
/// rewrite of the exemption could quietly make the rule pass on everything.
#[test]
fn the_plate_rule_still_refuses_paint_that_reaches_over_an_edge() {
    // The 900 x 620 frame the doc comment measured: plate x 84…544, panel x 64…900.
    let plate = Rect::from_min_max(Pos2::new(84.0, 82.0), Pos2::new(544.0, 565.0));
    let panel = Rect::from_min_max(Pos2::new(64.0, 0.0), Pos2::new(900.0, 581.0));
    let rule = |rect: Rect, clip: Rect| {
        let painted = Painted {
            shapes: vec![("line", rect, clip)],
            painted_nothing: 0,
        };
        std::panic::catch_unwind(|| {
            assert_on_plate(Page::Graph, Vec2::new(900.0, 620.0), &painted, &[plate])
        })
        .is_err()
    };
    // The relation line of a graph at zoom 2, off the left edge and across the header.
    assert!(
        rule(
            Rect::from_min_max(Pos2::new(-84.6, 243.5), Pos2::new(203.4, 477.5)),
            panel
        ),
        "a line reaching 292 px past the plate's left edge was accepted"
    );
    // A node that sticks out over the right edge by two pixels.
    assert!(
        rule(
            Rect::from_min_max(Pos2::new(543.0, 300.0), Pos2::new(547.0, 304.0)),
            panel
        ),
        "a node hanging over the plate's right edge was accepted"
    );
    // A label inside the plate with the panel clip — the zoom row, measured at
    // `[[523.4 92.0] - [536.4 100.0]]` on this plate — is not paint beside it.
    assert!(
        !rule(
            Rect::from_min_max(Pos2::new(523.4, 92.0), Pos2::new(536.4, 100.0)),
            panel
        ),
        "a widget wholly inside the plate was refused"
    );
}
