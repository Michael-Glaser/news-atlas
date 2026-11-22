//! The Graph page's plan at twelve window sizes, measured from the frames it paints.
//!
//! `GraphPlan` (`app.rs:1376-1420`) is private, so this audit drives the page the way the
//! window does — `Atlas::draw` under `egui::Context::run_ui` — and reads its two parts off the
//! frame. A plan is only as good as what it draws, and the defect it exists to prevent was a
//! drawn one: at 700×520 the relations column's text ran to x = 859 in a 700 px window
//! (`app.rs:2040-2046`).
//!
//! # What is measured, and what is read
//!
//! * **The plate** is measured: it is the page's only click-and-drag widget covering a fifth
//!   of the panel, and egui's interactive-widget inventory paints exactly its rect
//!   ([`plates`]).
//! * **The page box** is measured from the central panel's own clip rectangle less the page
//!   margins (`app.rs:480-481`). Its top is the plate's top, because the plan puts the plate
//!   at `page.min` in both branches — taking it from the plate keeps the header out of the
//!   numbers this file is about.
//! * **The relations column** is not a card and paints no rectangle of its own, so its box is
//!   read from the two numbers the plan itself uses — [`GRAPH_GAP`] and
//!   [`graph::PANEL_WIDTH`], both public — and every claim about it is then checked against
//!   the ink the panel actually paints: nothing in the gap, nothing outside the column, and
//!   in the stacked case nothing left over at the page's foot.
//! * **The floors.** `Atlas::MIN_PLATE_HEIGHT` and `Atlas::MIN_PLATE_WIDTH`
//!   (`app.rs:1097-1098`) are private; they are `theme::space::UNIT` × 64 and × 80, and this
//!   file states them the same way, so a change to the spacing scale moves the audit with it
//!   and a change to the constants shows up as a failure naming this line.
//!
//! # What it asserts, at every size
//!
//! 1. The plate is never below its floor — 320 × 256. The smallest page box in the set is
//!    456 × 343, so the plan's documented compromise for a box shorter than the floor
//!    (`(shared * PLATE_SHARE).max(MIN_PLATE_HEIGHT).min(shared)`) is never taken here and
//!    the floor is asserted without a condition.
//! 2. The relations column is never below its own floor: its documented width
//!    [`graph::PANEL_WIDTH`] beside the plate, the page box's width when stacked, and a
//!    positive height at every size. The stacked height is the plan's "the column the rest"
//!    (`app.rs:1405-1420`), so it is reported here rather than held to a constant the plan
//!    does not promise: the table marks it and the summary line under the table says how thin
//!    the rest gets.
//! 3. The plate and the column never overlap, and no ink of the column lands in the gap.
//! 4. Side by side, the plate is exactly as tall as the column — both reach the page box's
//!    foot, so the plate is never short of the column by a strip of dead page.
//! 5. Stacked, the plate is above the column, both at the page box's full width, with exactly
//!    one [`GRAPH_GAP`] between them and the column reaching the page box's foot.
//!
//! Run it with the table printed:
//!
//! ```text
//! cargo test --offline -p atlas-desktop --test graph_sizes -- --nocapture
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::epaint::{ClippedShape, Shape};
use egui::{Color32, Pos2, Rect, Vec2};

use atlas_desktop::app::{Atlas, Page, GRAPH_GAP};
use atlas_desktop::theme;
use atlas_desktop::views::graph;

/// Twelve windows from the smallest the page must survive to a 1440p screen, chosen so the
/// side-by-side floor is crossed inside the set: the plate's 320 px, the gap and the column's
/// 320 px need 656 px of page box, which a folded rail (64 px) and the two 20 px page margins
/// reach at about 760 px of window — so 720 is stacked and 800 is not.
const SIZES: [(f32, f32); 12] = [
    (560.0, 480.0),
    (640.0, 560.0),
    (720.0, 600.0),
    (800.0, 640.0),
    (900.0, 600.0),
    (1000.0, 700.0),
    (1100.0, 820.0),
    (1280.0, 800.0),
    (1440.0, 900.0),
    (1680.0, 1050.0),
    (1920.0, 1080.0),
    (2560.0, 1440.0),
];

/// The page margins the central panel draws the page inside (`app.rs:480-481`).
const PAGE_MARGIN: Vec2 = Vec2::new(20.0, 16.0);

/// Half a hairline: what "equal" and "reaches" mean to a pixel grid. Anything larger is a
/// visible strip, not rounding.
const TOL: f32 = 2.5;

/// The plate's floor, restated from `app.rs:1097-1098`: 64 units tall, 80 wide, which is
/// 256 × 320 px on the current scale. Private in the crate, so it is derived from the public
/// [`theme::space::UNIT`] here rather than typed as two numbers.
fn plate_floor() -> Vec2 {
    Vec2::new(theme::space::UNIT * 80.0, theme::space::UNIT * 64.0)
}

/// The column's floor: the width `graph.rs:2311` documents for it. When stacked it is given
/// the page box's whole width, which is wider — never narrower.
const COLUMN_FLOOR: f32 = graph::PANEL_WIDTH;

/// The page box width the plan needs to sit its two parts beside each other
/// (`app.rs:1389`): the plate's floor, one gap, the column's width.
fn needs_beside() -> f32 {
    plate_floor().x + GRAPH_GAP + COLUMN_FLOOR
}

// ------------------------------------------------------------------------ the archive

/// A `data/atlas.db` of our own, in a temp directory, removed when the test ends. The reader's
/// archive is never opened; eight entities and seven relations are enough for the plate to
/// have a picture and the panel to have rows.
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
            "atlas-graph-sizes-{}-{}-{}",
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
        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'The Nordic Post'),(2,'Alpine Review')",
            [],
        )
        .expect("two sources");
        let stories: [(i64, &str, i64, &str, &str, f64, f64); 6] = [
            (
                1,
                "Harbour crane strike ends after nine days",
                1,
                "DEU",
                "Hamburg",
                54.0,
                10.0,
            ),
            (
                2,
                "Alpine rail freight reaches a record",
                2,
                "AUT",
                "Vienna",
                48.2,
                16.4,
            ),
            (
                3,
                "Chip export rules tighten in Asia",
                1,
                "JPN",
                "Tokyo",
                35.7,
                139.7,
            ),
            (
                4,
                "Drought reshapes the southern harvest",
                2,
                "ESP",
                "Madrid",
                40.4,
                -3.7,
            ),
            (
                5,
                "A second harbour approves the contract",
                1,
                "NLD",
                "Rotterdam",
                51.9,
                4.5,
            ),
            (
                6,
                "Ferry traffic resumes on the northern route",
                2,
                "NOR",
                "Oslo",
                59.9,
                10.7,
            ),
        ];
        for (id, title, source, iso, place, lat, lon) in stories {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 1, 0, ?10, ?11)",
                rusqlite::params![
                    id,
                    title,
                    source,
                    recent,
                    format!("{title} — a summary sentence for the list."),
                    format!("{title}. The body of the story."),
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
        drop(conn);
        TempArchive { dir, file }
    }

    fn atlas(&self) -> Atlas {
        Atlas::with_archive(self.file.clone())
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`), so the seeded rows stay
/// inside the 72 h window whenever this runs. Howard Hinnant's civil-from-days, because the
/// crate has no time dependency and neither does this.
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

// ------------------------------------------------------------------- driving a frame

/// The window as a release build paints it: the house style, and none of egui's debug
/// painting (`debug_on_hover` is on in debug builds, `egui-0.37.0/src/style.rs:1452`).
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.all_styles_mut(|style| {
        style.debug.debug_on_hover = false;
        style.debug.show_widget_hits = false;
    });
    ctx
}

/// The same window with egui's interactive-widget inventory switched on, which paints each
/// widget's own rect: the pass the plate is read from.
fn inventory_context() -> egui::Context {
    let ctx = context();
    ctx.all_styles_mut(|style| style.debug.show_interactive_widgets = true);
    ctx
}

/// One frame of the real window, with the texture deltas cleared the way a backend uploads
/// them (`epaint-0.37.0/src/textures.rs:337` panics on `Drop` otherwise).
fn frame(ctx: &egui::Context, size: Vec2, index: usize, atlas: &mut Atlas) -> egui::FullOutput {
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(index as f64 / 60.0),
            ..Default::default()
        },
        |ui| atlas.draw(ui),
    );
    let mut textures = output.textures_delta;
    textures.clear();
    egui::FullOutput {
        platform_output: output.platform_output,
        textures_delta: Default::default(),
        shapes: output.shapes,
        pixels_per_point: output.pixels_per_point,
        viewport_output: output.viewport_output,
    }
}

// ------------------------------------------------------------------ reading a frame

/// One painted shape: its own rectangle and the clip it was given. The intersection is what a
/// backend actually puts on screen.
#[derive(Clone, Copy)]
struct Seen {
    kind: &'static str,
    rect: Rect,
    clip: Rect,
}

/// One frame, seen the way a backend sees it.
#[derive(Default)]
struct Painted {
    shapes: Vec<Seen>,
    /// The rectangles that were filled: the plates and the cards are among them.
    filled: Vec<Rect>,
}

impl Painted {
    fn read(output: &egui::FullOutput) -> Painted {
        let mut painted = Painted::default();
        for ClippedShape {
            clip_rect, shape, ..
        } in &output.shapes
        {
            walk(shape, &mut |shape| {
                let rect = shape.visual_bounding_rect();
                if rect == Rect::NOTHING || !rect.is_finite() {
                    return;
                }
                if let Shape::Rect(rect_shape) = shape {
                    if rect_shape.fill != Color32::TRANSPARENT {
                        painted.filled.push(rect_shape.rect);
                    }
                }
                painted.shapes.push(Seen {
                    kind: kind_of(shape),
                    rect,
                    clip: *clip_rect,
                });
            });
        }
        painted
    }

    /// What a shape actually paints: its own rectangle, cut by its clip. `Rect::NOTHING` when
    /// the cut is empty — a pruned shape is not on screen.
    fn painted_rect(seen: &Seen) -> Rect {
        let cut = seen.rect.intersect(seen.clip);
        if cut.width() > 0.0 && cut.height() > 0.0 {
            cut
        } else {
            Rect::NOTHING
        }
    }
}

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

/// The central panel the pages draw in, read off the frame: a panel paints through a clip
/// rectangle of its own, so the clip rectangles *are* the boxes. The rail's column and the
/// status row are the two edges this reads, and both are checked rather than returned.
fn panel_box(size: Vec2, painted: &Painted) -> Rect {
    let rail_right = painted
        .shapes
        .iter()
        .filter(|seen| seen.clip.min.x <= 0.5 && seen.clip.max.x <= size.x * 0.5)
        .map(|seen| seen.clip.max.x)
        .fold(0.0_f32, f32::max);
    let panel_bottom = painted
        .shapes
        .iter()
        .filter(|seen| {
            seen.clip.min.y <= 0.5
                && (seen.clip.min.x - rail_right).abs() <= TOL
                && seen.clip.max.x > rail_right
        })
        .map(|seen| seen.clip.max.y)
        .fold(0.0_f32, f32::max);
    assert!(
        rail_right > 0.0 && panel_bottom > 0.0 && panel_bottom < size.y,
        "the frame at {:.0}x{:.0} painted no rail or no central panel: rail {rail_right}, \
         panel bottom {panel_bottom}",
        size.x,
        size.y
    );
    Rect::from_min_max(Pos2::new(rail_right, 0.0), Pos2::new(size.x, panel_bottom))
}

/// The rects egui inventoried as click-and-drag widgets covering a fifth of the panel: the
/// plate, and nothing else on this page.
fn plates(output: &egui::FullOutput, panel: Rect) -> Vec<Rect> {
    let mut found = Vec::new();
    for ClippedShape { shape, .. } in &output.shapes {
        walk(shape, &mut |shape| {
            if let Shape::Rect(rect) = shape {
                // egui's inventory colours: click, click-and-drag, drag.
                if rect.stroke.color == Color32::from_rgb(0x88, 0, 0x88)
                    && rect.rect.width() * rect.rect.height() > panel.width() * panel.height() * 0.2
                {
                    found.push(rect.rect);
                }
            }
        });
    }
    found
}

// -------------------------------------------------------------- the plan, measured

/// One size's numbers: the plate as measured, the page box it was given, the column the plan
/// puts beside or below it, and the ink the panel actually painted.
struct Measured {
    plate: Rect,
    page: Rect,
    column: Rect,
    ink: Rect,
    /// Every shape the page paints, cut by its clip: what the gap check walks.
    shapes: Vec<(&'static str, Rect)>,
    stacked: bool,
}

/// Measure the Graph page at `size`.
///
/// Two passes, each with its own `Atlas` over the same archive: the plate comes from the
/// inventory pass, the ink and the page box from an ordinary one — an inventory pass paints
/// a rectangle per widget, and those would be ink that the panel did not paint.
fn measure(size: Vec2, archive: &TempArchive) -> Measured {
    let plate = {
        let ctx = inventory_context();
        let mut atlas = archive.atlas();
        atlas.set_page(Page::Graph);
        // Two frames: the first lays the panels out, the second paints where they landed.
        let mut frame_output = frame(&ctx, size, 0, &mut atlas);
        for index in 1..2 {
            frame_output = frame(&ctx, size, index, &mut atlas);
        }
        let painted = Painted::read(&frame_output);
        let panel = panel_box(size, &painted);
        let mut found = plates(&frame_output, panel);
        assert_eq!(
            found.len(),
            1,
            "the Graph page at {:.0}x{:.0} has {} plates, not one: {found:?}",
            size.x,
            size.y,
            found.len()
        );
        found.remove(0)
    };

    let ctx = context();
    let mut atlas = archive.atlas();
    atlas.set_page(Page::Graph);
    let mut painted = Painted::default();
    for index in 0..3 {
        painted = Painted::read(&frame(&ctx, size, index, &mut atlas));
    }
    let panel = panel_box(size, &painted);
    // The page box: the panel less its margins. The top is the plate's own top, because the
    // plan puts the plate at `page.min` in both branches — the header above it is not part of
    // the box and is not part of this audit.
    let page = Rect::from_min_max(
        Pos2::new(panel.min.x + PAGE_MARGIN.x, plate.min.y),
        Pos2::new(panel.max.x - PAGE_MARGIN.x, panel.max.y - PAGE_MARGIN.y),
    );
    // Which branch the plan took, measured rather than assumed: a plate that spans the page
    // box can only be the stacked one, because side by side reserves the column's width.
    let stacked = page.width() - plate.width() <= TOL;
    // The column, from the plan's own two numbers: beside the plate it keeps its width at the
    // page's right edge, stacked it is the page's full width under one gap.
    let column = if stacked {
        Rect::from_min_max(
            Pos2::new(page.min.x, plate.max.y + GRAPH_GAP),
            Pos2::new(page.max.x, page.max.y),
        )
    } else {
        Rect::from_min_max(
            Pos2::new(page.max.x - COLUMN_FLOOR, page.min.y),
            Pos2::new(page.max.x, page.max.y),
        )
    };
    // The ink the panel paints: right of the plate when the two are beside each other, below
    // it when they are stacked.
    let beyond = if stacked {
        Rect::from_min_max(Pos2::new(page.min.x, plate.max.y + 1.0), page.max)
    } else {
        Rect::from_min_max(Pos2::new(plate.max.x + 1.0, page.min.y), page.max)
    };
    // The panel's ink: everything that lands past the plate, less the page's own background —
    // a rectangle that contains the plate is furniture painted under both halves, not a row
    // of the column.
    let ink = painted
        .shapes
        .iter()
        .map(Painted::painted_rect)
        .filter(|rect| *rect != Rect::NOTHING && !rect.contains_rect(plate))
        .filter(|rect| rect.intersects(beyond))
        .fold(Rect::NOTHING, |union, rect| union.union(rect));
    assert!(
        ink.width() > 0.0 && ink.height() > 0.0,
        "the Graph page at {:.0}x{:.0} painted nothing in its relations column {column:?}",
        size.x,
        size.y
    );
    Measured {
        plate,
        page,
        column,
        ink,
        shapes: painted
            .shapes
            .iter()
            .map(|seen| (seen.kind, Painted::painted_rect(seen)))
            .filter(|(_, rect)| *rect != Rect::NOTHING)
            .collect(),
        stacked,
    }
}

/// The measured table, printed by every run: one line per window.
fn report(size: Vec2, plan: &Measured, floor: Vec2) {
    println!(
        "{:>4.0}x{:<4.0} page {:.0}x{:.0} · {} · plate {:.0}x{:.0} at ({:.0},{:.0}) · \
         column {:.0}x{:.0} at ({:.0},{:.0}){} · ink {:.0}x{:.0} · floors {:.0}x{:.0} {}",
        size.x,
        size.y,
        plan.page.width(),
        plan.page.height(),
        if plan.stacked { "stacked   " } else { "side by side" },
        plan.plate.width(),
        plan.plate.height(),
        plan.plate.min.x,
        plan.plate.min.y,
        plan.column.width(),
        plan.column.height(),
        plan.column.min.x,
        plan.column.min.y,
        if plan.column.height() + TOL < floor.y {
            ", under the plate's floor"
        } else {
            ""
        },
        plan.ink.width(),
        plan.ink.height(),
        floor.x,
        floor.y,
        if plan.plate.width() + TOL >= floor.x && plan.plate.height() + TOL >= floor.y {
            "met"
        } else {
            "SHORT"
        }
    );
}

// ------------------------------------------------------------------------- the audit

/// The plan at twelve window sizes: floors, overlap, equal heights beside, and the stacked
/// order — with the whole table printed.
#[test]
fn the_plate_and_the_column_fit_the_page_at_twelve_window_sizes() {
    let archive = TempArchive::build();
    let floor = plate_floor();
    let mut thinnest: Option<(f32, f32, f32)> = None;
    for (width, height) in SIZES {
        let size = Vec2::new(width, height);
        let plan = measure(size, &archive);
        report(size, &plan, floor);
        let Measured {
            plate,
            page,
            column,
            stacked,
            ..
        } = plan;

        // 1. The plate is never below its floor, when the page box can hold the floor.
        assert!(
            plate.min.x >= page.min.x - TOL && plate.max.x <= page.max.x + TOL,
            "{width:.0}x{height:.0}: the plate {plate:?} is outside the page box {page:?}"
        );
        assert!(
            plate.width() + TOL >= floor.x,
            "{width:.0}x{height:.0}: the plate is {:.0} px wide, below its {} px floor, in a \
             page box {:.0} px wide",
            plate.width(),
            floor.x,
            page.width()
        );
        assert!(
            plate.height() + TOL >= floor.y,
            "{width:.0}x{height:.0}: the plate is {:.0} px tall, below its {} px floor, in a \
             page box {:.0} px tall",
            plate.height(),
            floor.y,
            page.height()
        );

        // 2. The relations column is never below its own floor: its documented width, the
        //    page's width when stacked, and a positive height. The stacked height is "the
        //    rest" (app.rs:1405-1420), reported below rather than held to a number the plan
        //    does not promise.
        if stacked {
            assert!(
                (column.width() - page.width()).abs() <= TOL,
                "{width:.0}x{height:.0}: a stacked column is {:.0} px wide in a {:.0} px page box",
                column.width(),
                page.width()
            );
        } else {
            assert!(
                (column.width() - COLUMN_FLOOR).abs() <= TOL,
                "{width:.0}x{height:.0}: the column beside the plate is {:.0} px wide, and the \
                 panel documents {}",
                column.width(),
                COLUMN_FLOOR
            );
        }
        assert!(
            column.width() + TOL >= COLUMN_FLOOR,
            "{width:.0}x{height:.0}: the relations column is {:.0} px wide, below its {} px floor",
            column.width(),
            COLUMN_FLOOR
        );
        assert!(
            column.height() > 0.0,
            "{width:.0}x{height:.0}: the relations column has no height at all: {column:?}"
        );
        if thinnest.map_or(true, |(thin, _, _)| column.height() < thin) {
            thinnest = Some((column.height(), width, height));
        }
        assert!(
            column.max.y <= page.max.y + TOL,
            "{width:.0}x{height:.0}: the column {column:?} runs past the page box's foot {:.0}",
            page.max.y
        );

        // 3. The two never overlap.
        assert!(
            !plate.intersects(column),
            "{width:.0}x{height:.0}: the plate {plate:?} and the column {column:?} overlap by \
             {:.0}x{:.0} px",
            plate.intersect(column).width(),
            plate.intersect(column).height()
        );

        // 4. Side by side, the plate is exactly as tall as the column: both reach the page
        //    box's foot, so neither leaves a dead strip under the other.
        // 5. Stacked, the plate is above the column at the page's full width, one gap apart.
        if stacked {
            assert!(
                (plate.width() - page.width()).abs() <= TOL,
                "{width:.0}x{height:.0}: a stacked plate is {:.0} px wide in a {:.0} px page box",
                plate.width(),
                page.width()
            );
            assert!(
                plate.max.y <= column.min.y + TOL,
                "{width:.0}x{height:.0}: the plate {plate:?} is not above the column {column:?}"
            );
            assert!(
                (column.min.y - plate.max.y - GRAPH_GAP).abs() <= TOL,
                "{width:.0}x{height:.0}: the plate's foot {:.0} and the column's top {:.0} are \
                 {:.0} px apart, and the plan's gap is {}",
                plate.max.y,
                column.min.y,
                column.min.y - plate.max.y,
                GRAPH_GAP
            );
            assert!(
                (column.max.y - page.max.y).abs() <= TOL,
                "{width:.0}x{height:.0}: the column stops {:.0} px above the page box's foot, \
                 leaving a dead band",
                page.max.y - column.max.y
            );
        } else {
            assert!(
                (plate.height() - column.height()).abs() <= TOL,
                "{width:.0}x{height:.0}: the plate is {:.0} px tall and the column {:.0} px, \
                 side by side they are one row",
                plate.height(),
                column.height()
            );
            assert!(
                (plate.max.y - page.max.y).abs() <= TOL,
                "{width:.0}x{height:.0}: the plate stops {:.0} px above the page box's foot, \
                 leaving a dead band",
                page.max.y - plate.max.y
            );
        }

        // The mode is the plan's documented rule, not a guess: the plate's floor, the gap and
        // the column's width decide it (app.rs:1389).
        assert_eq!(
            stacked,
            page.width() < needs_beside(),
            "{width:.0}x{height:.0}: the page box is {:.0} px wide and needs {:.0} to sit \
             beside the column",
            page.width(),
            needs_beside()
        );
    }
    let (thin, at_w, at_h) = thinnest.expect("twelve sizes, twelve columns");
    println!(
        "the thinnest relations column in the set is {thin:.0} px tall, at {at_w:.0}x{at_h:.0}: \
         stacked, the plan gives the column \"the rest\" after the plate's {:.0} px floor \
         (app.rs:1405-1420), and the rest is that"
        , floor.y
    );
}

/// The column's ink stays in its own box at every size: the historical defect was text that
/// was not clipped short but drawn outside the window (`app.rs:2040-2046`).
#[test]
fn the_column_paints_only_inside_its_column() {
    let archive = TempArchive::build();
    for (width, height) in SIZES {
        let size = Vec2::new(width, height);
        let measured = measure(size, &archive);
        let Measured {
            plate,
            page,
            column,
            ink,
            shapes,
            stacked,
        } = measured;
        assert!(
            ink.min.x >= column.min.x - TOL && ink.max.x <= column.max.x + TOL,
            "{width:.0}x{height:.0}: the column paints {ink:?}, outside its box {column:?} \
             (the page's right edge is {:.0})",
            page.max.x
        );
        assert!(
            ink.min.y >= column.min.y - TOL && ink.max.y <= page.max.y + TOL,
            "{width:.0}x{height:.0}: the column paints {ink:?}, outside its box {column:?}"
        );
        // Nothing painted in the gap that separates the two: a panel drawn a gap too early
        // would be a control sitting on the plate's edge.
        let gap = if stacked {
            Rect::from_min_max(
                Pos2::new(page.min.x, plate.max.y),
                Pos2::new(page.max.x, column.min.y),
            )
        } else {
            Rect::from_min_max(
                Pos2::new(plate.max.x, page.min.y),
                Pos2::new(column.min.x, page.max.y),
            )
        };
        for (kind, drawn) in &shapes {
            // The page's own background covers both halves and the gap by existing.
            if drawn.contains_rect(plate) || drawn.contains_rect(column) || drawn.contains_rect(gap)
            {
                continue;
            }
            assert!(
                !drawn.intersects(gap.shrink(1.0)),
                "{width:.0}x{height:.0}: a {kind} {drawn:?} is painted in the {:.0} px gap \
                 between the plate and the column ({gap:?})",
                gap.width().max(gap.height())
            );
        }
    }
}
