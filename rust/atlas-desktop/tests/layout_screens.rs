//! Three windows, two canvases, and the numbers that say they fit.
//!
//! A canvas is the one place a page hands a rectangle to a painter and lets it draw
//! whatever it likes — the world map on the Overview and the relation graph on the Graph
//! page. "It looks squeezed" is not a report a test can act on, so this file turns the two
//! complaints into properties with rectangles:
//!
//! 1. **Each canvas gets the content box minus the page's own chrome, and nothing else.**
//!    The content box is the central panel less the page margins; the chrome is whatever the
//!    page draws above the canvas (the heading, and on the Overview the map card's title row
//!    and its margins). The number the box has left is the number the canvas is given, so a
//!    dead band above or below it, a scrollbar that ate a strip of it, or a canvas that
//!    stopped short of the page's edge all fail here. The graph's page is the tighter claim
//!    of the two: its canvas and its relations column are the *only* things under the
//!    heading, so the canvas must reach the page's bottom edge exactly.
//! 2. **The graph's relations column does not sit on a node.** The column is the detail
//!    panel; a node under it would be a node the reader cannot click. The plate and the
//!    column must be disjoint, nothing may be painted in the gap between them, and no node
//!    may have any part inside the column. The panel is rows of text and controls rather than
//!    a card, so what is measured is the ink it paints: it must start at the column's own left
//!    edge and stay inside the column, both for the one-line hint it shows with nothing
//!    selected and for the longest state there is, an entity open with its relations listed.
//! 3. **A page switch keeps each camera on its own view.** The map's zoom and the graph's
//!    zoom and pan belong to the view that owns them: zoom the map, walk to the Graph page
//!    and back, and the map is where it was left; pan and zoom the graph, walk to the
//!    Overview and back, and the graph is where it was left. A view rebuilt on page entry
//!    fails this, and a camera shared between the two views fails it the other way.
//!
//! # How it is driven, and what is measured
//!
//! `Atlas::with_archive` opens a temporary archive built by [`TempArchive`] and `Atlas::draw`
//! is given the root `Ui` from `egui::Context::run_ui` — the same call `eframe` makes, with a
//! screen rectangle, a clock and synthetic wheel and pointer events instead of a GPU surface.
//! No window is opened, no port is touched, nothing is downloaded.
//!
//! Every rectangle is *read off the frame*, never re-derived from the layout's arithmetic:
//!
//! * the rail's column and the status row from the clip rectangles the panels paint through
//!   ([`boxes`]), which is how the content box is found;
//! * the two canvases from egui's own interactive-widget inventory — a widget with the
//!   click-and-drag sense that covers a fifth of the panel is a plate and nothing else
//!   ([`canvases`]);
//! * the map's zoom from the caption it paints on the plate (`zoom ×1.4 · …`), and a camera's
//!   motion from the dots it paints, which is all a page shows a reader.
//!
//! Run it, with the measured table printed:
//!
//! ```text
//! cargo test --offline -p atlas-desktop --test layout_screens -- --nocapture
//! ```
//!
//! The numbers the assertions are written against are printed by every run, so a change in
//! them is a change to this file and not a mystery in a failure message.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::epaint::{ClippedShape, Shape};
use egui::{Color32, Pos2, Rect, Vec2};

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::theme;
use atlas_desktop::views::{graph, overview};

/// The three windows the audit is taken at: the size `main.rs` opens at and a taller one, a
/// laptop, and the smallest window the layout must still fit in one screen.
const SIZES: [(f32, f32); 3] = [(1280.0, 800.0), (1100.0, 820.0), (900.0, 600.0)];

/// The page margins the central panel is drawn inside (`app.rs:480-481`). The content box is
/// the central panel less these, and the heading is drawn from its top-left corner — which is
/// asserted, not assumed ([`content_box`]).
const PAGE_MARGIN: Vec2 = Vec2::new(20.0, 16.0);

/// The rail's two widths: folded to marks, and open with its words (`app.rs:484-487`). The
/// rail folds itself below 1 068 px of window width, so 1 280 gets the wide one and the two
/// narrower screens the folded one.
const RAILS: [f32; 2] = [64.0, 188.0];

/// A card's own chrome on each side: its 1 px border plus its 12 px inner margin
/// (`views/mod.rs`, `card`). The Overview's plate sits inside one of these on both sides.
const CARD_CHROME: f32 = 13.0;

/// Rounding: egui snaps hairlines to half pixels, and an inventory rectangle is read back
/// with the stroke it was drawn with.
const TOL: f32 = 2.5;

// --------------------------------------------------------------- the archive

/// A `data/atlas.db` of our own, in a temp directory, removed when the test ends. The live
/// archive is never opened: a test that read the reader's history would be neither hermetic
/// nor repeatable. The rows are enough for both canvases to have something on them — six
/// places across the world, eight entities and seven relations.
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
            "atlas-layout-screens-{}-{}-{}",
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
        // World-wide coordinates, so a node or a dot is never all in the same corner of
        // either plate: Hamburg, Vienna, Tokyo, Madrid, Rotterdam, Oslo.
        let stories: [(i64, &str, i64, &str, &str, f64, f64); 6] = [
            (1, "Harbour crane strike ends after nine days", 1, "DEU", "Hamburg", 54.0, 10.0),
            (2, "Alpine rail freight reaches a record", 2, "AUT", "Vienna", 48.2, 16.4),
            (3, "Chip export rules tighten in Asia", 1, "JPN", "Tokyo", 35.7, 139.7),
            (4, "Drought reshapes the southern harvest", 2, "ESP", "Madrid", 40.4, -3.7),
            (5, "A second harbour approves the contract", 1, "NLD", "Rotterdam", 51.9, 4.5),
            (6, "Ferry traffic resumes on the northern route", 2, "NOR", "Oslo", 59.9, 10.7),
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

// ------------------------------------------------------------- driving a frame

/// The window as a release build paints it: the house style, and none of egui's debug
/// painting. `debug_on_hover` is off because egui turns it **on in debug builds**
/// (`egui-0.37.0/src/style.rs:1452`, `cfg!(debug_assertions)`).
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.all_styles_mut(|style| {
        style.debug.debug_on_hover = false;
        style.debug.show_widget_hits = false;
    });
    ctx
}

/// The same window with egui's interactive-widget inventory switched on: the pass the canvas
/// rectangles are read from. `debug_painting` (`egui-0.37.0/src/context.rs:2988`) paints a
/// `debug_rect` per interactive widget in one of three sense colours (`context.rs:3013`), and
/// those rectangles *are* the widgets' rects.
fn inventory_context() -> egui::Context {
    let ctx = context();
    ctx.all_styles_mut(|style| style.debug.show_interactive_widgets = true);
    ctx
}

/// The gap the theme puts between two stacked widgets (`theme::install`), read from the style
/// rather than typed here: every page's own spacer is measured on top of it.
fn item_gap() -> f32 {
    context().global_style().spacing.item_spacing.y
}

fn raw_input(size: Vec2, index: usize, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(index as f64 / 60.0),
        events,
        ..Default::default()
    }
}

/// One frame of the real window, with the texture deltas cleared the way a backend uploads
/// them (`epaint-0.37.0/src/textures.rs:337` panics on `Drop` otherwise).
fn frame(
    ctx: &egui::Context,
    size: Vec2,
    index: usize,
    atlas: &mut Atlas,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let output = ctx.run_ui(raw_input(size, index, events), |ui| atlas.draw(ui));
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

// ---------------------------------------------------------------- reading a frame

/// One painted shape: what it is, the rectangle it draws into, and the clip rectangle it was
/// given. The intersection is what a backend actually puts on screen.
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
    /// Every galley that carried text, with the rectangle it was drawn into.
    text: Vec<(String, Rect)>,
    /// The centres of the small circles: pins on the map, nodes on the graph.
    dots: Vec<Pos2>,
    /// The rectangles that were filled, with their fill: the plates and the cards are among
    /// them, and a scrollbar thumb would be too.
    filled: Vec<(Rect, Color32)>,
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
                match shape {
                    Shape::Text(text) => {
                        if !text.galley.text().is_empty() {
                            painted
                                .text
                                .push((text.galley.text().to_owned(), rect));
                        }
                    }
                    Shape::Circle(circle) => {
                        // A dot is a filled circle with a radius a marker has; a ring drawn
                        // around one is a stroke, and is not counted twice.
                        if circle.fill != Color32::TRANSPARENT && circle.radius <= 24.0 {
                            painted.dots.push(circle.center);
                        }
                    }
                    Shape::Rect(rect_shape) => {
                        if rect_shape.fill != Color32::TRANSPARENT {
                            painted.filled.push((rect_shape.rect, rect_shape.fill));
                        }
                    }
                    _ => {}
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

    /// What a shape actually paints: its own rectangle, cut by its clip. `Rect::NOTHING`
    /// when the cut is empty — a pruned shape is not on screen.
    fn painted_rect(seen: &Seen) -> Rect {
        let cut = seen.rect.intersect(seen.clip);
        if cut.width() > 0.0 && cut.height() > 0.0 {
            cut
        } else {
            Rect::NOTHING
        }
    }

    /// The zoom caption the map paints on its plate, if it painted one: `zoom ×1.4 · …`.
    fn zoom_caption(&self) -> Option<&str> {
        self.text
            .iter()
            .find(|(text, _)| text.starts_with("zoom ×"))
            .map(|(text, _)| text.as_str())
    }

    /// The text a page painted, one line per galley.
    fn words(&self) -> String {
        self.text
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The shapes that belong to the page rather than to the window.
    ///
    /// The rail, its hairline and the status row are painted in the same frame and are not the
    /// page: each of them starts left of the page's columns or below its bottom edge, and this is
    /// the one place that is said. Everything else is the page's own paint.
    fn page_shapes<'a>(&'a self, content: Rect) -> impl Iterator<Item = (&'static str, Rect)> + 'a {
        self.shapes
            .iter()
            .map(|seen| (seen.kind, Painted::painted_rect(seen)))
            .filter(move |(_, rect)| {
                *rect != Rect::NOTHING
                    && rect.min.x >= content.min.x - TOL
                    && rect.max.x <= content.max.x + TOL
                    && rect.min.y >= content.min.y - TOL
            })
    }

    /// The lowest pixel the page reaches: equal to the content box's bottom edge when the page
    /// fills the box it was given, short of it when there is a dead band, past it when the page
    /// overflows.
    fn page_bottom(&self, content: Rect) -> f32 {
        self.page_shapes(content)
            .map(|(_, rect)| rect.max.y)
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// The rectangle a galley with `starts` in it was drawn into, the first one at or right of
    /// `not_left_of`. The rail names every page too, so a lookup that ignored where the text is
    /// would find the nav row and call it the title.
    fn text_rect(&self, starts: &str, not_left_of: f32) -> Option<Rect> {
        self.text
            .iter()
            .find(|(text, rect)| {
                text.starts_with(starts) && rect.min.x >= not_left_of - 0.5
            })
            .map(|(_, rect)| *rect)
    }

    /// The dots painted inside `area`: a canvas's own markers, without the rail's mark and the
    /// status row's.
    fn dots_in(&self, area: Rect) -> Vec<Pos2> {
        self.dots
            .iter()
            .copied()
            .filter(|at| area.contains(*at))
            .collect()
    }

    /// `area`'s dots in a fixed order, so two frames of the same picture compare equal and two
    /// pictures compare by count and by distance. Sorting by x then y is all the identity a set
    /// of painted centres needs.
    fn dot_set(&self, area: Rect) -> Vec<Pos2> {
        let mut dots = self.dots_in(area);
        dots.sort_by(|a, b| {
            a.x.partial_cmp(&b.x)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
        });
        dots
    }

    /// The centre of `area`'s dots — one vector, so "has the picture moved" is one comparison.
    fn dot_middle(&self, area: Rect) -> Vec2 {
        let dots = self.dots_in(area);
        if dots.is_empty() {
            return Vec2::ZERO;
        }
        let sum = dots.iter().fold(Vec2::ZERO, |sum, at| sum + at.to_vec2());
        sum / dots.len() as f32
    }

    /// How far `area`'s dots spread from one another: the width and height of their box. A zoom
    /// multiplies it, a pan does not.
    fn dot_spread(&self, area: Rect) -> Vec2 {
        let dots = self.dots_in(area);
        if dots.is_empty() {
            return Vec2::ZERO;
        }
        let min = dots
            .iter()
            .fold(Pos2::new(f32::INFINITY, f32::INFINITY), |low, at| {
                Pos2::new(low.x.min(at.x), low.y.min(at.y))
            });
        let max = dots
            .iter()
            .fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |high, at| {
                Pos2::new(high.x.max(at.x), high.y.max(at.y))
            });
        max - min
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

// ------------------------------------------------------------------ the boxes

/// The central panel the pages draw in, read off the frame rather than remembered.
///
/// A panel paints through a clip rectangle of its own, so the clip rectangles *are* the boxes:
/// everything clipped to the rail ends at the rail's right edge, and everything clipped to the
/// central panel starts there and ends where the status row begins. The rail's own column and
/// the status row are the two edges this reads; they are checked, not returned, because every
/// number below is measured from the panel.
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
    assert!(
        RAILS
            .iter()
            .any(|width| (width - rail_right).abs() <= TOL),
        "the rail is {rail_right} px wide at {:.0}x{:.0}, and the app documents two widths, \
         {RAILS:?} — a third one is a change to state on purpose",
        size.x,
        size.y
    );
    Rect::from_min_max(Pos2::new(rail_right, 0.0), Pos2::new(size.x, panel_bottom))
}

/// The content box: the central panel less the page margins the window draws it inside.
///
/// The heading is the proof. A page starts with its title at the box's top-left corner, so the
/// title's own galley must sit on the box's left edge — if the margins in this file ever stop
/// being the margins the window uses, the title moves and this fails instead of every number
/// below being quietly off by the difference. The top is taken from the title rather than from
/// the margin because a label's galley is drawn a few pixels below its row's top: the offset is
/// printed by every run and only bounded here, so it cannot drift into a second layout.
fn content_box(size: Vec2, painted: &Painted, page: Page) -> Rect {
    let panel = panel_box(size, painted);
    let title = painted
        .text_rect(page.label(), panel.min.x)
        .unwrap_or_else(|| panic!("{} painted no title", page.label()));
    let offset = title.min - panel.min;
    assert!(
        (offset.x - PAGE_MARGIN.x).abs() <= TOL,
        "{}: the title starts {:.1} px into the panel, and the page margin is {:.1} — the \
         margins this file documents are not the ones the window uses",
        page.label(),
        offset.x,
        PAGE_MARGIN.x
    );
    assert!(
        offset.y >= 0.0 && offset.y <= PAGE_MARGIN.y + 8.0,
        "{}: the title starts {:.1} px below the panel's top, and the page margin is {:.1}",
        page.label(),
        offset.y,
        PAGE_MARGIN.y
    );
    println!(
        "{} {:.0}x{:.0}: title {:.1} px into the panel · panel {:.1?}",
        page.label(),
        size.x,
        size.y,
        offset.y,
        panel.size()
    );
    Rect::from_min_max(
        Pos2::new(panel.min.x + PAGE_MARGIN.x, title.min.y),
        panel.max - PAGE_MARGIN,
    )
}

/// The canvases on the page: widgets egui inventoried with the click-and-drag sense that cover
/// a fifth of the panel, which no control but a plate reaches (`map.rs`, `graph.rs`).
fn canvases(size: Vec2, page: Page, archive: &TempArchive) -> (Painted, Vec<Rect>) {
    let ctx = inventory_context();
    let mut atlas = archive.atlas();
    atlas.set_page(page);
    // Two frames: the first lays the panels out, the second paints where they landed.
    frame(&ctx, size, 0, &mut atlas, Vec::new());
    let output = frame(&ctx, size, 1, &mut atlas, Vec::new());
    let painted = Painted::read(&output);
    let panel = panel_box(size, &painted);
    let mut canvases = Vec::new();
    for ClippedShape { shape, .. } in &output.shapes {
        walk(shape, &mut |shape| {
            if let Shape::Rect(rect) = shape {
                // egui's inventory colours: click, click-and-drag, drag.
                let colour = rect.stroke.color;
                // A widget scrolled out of its clip rect is inventoried with an empty rect
                // (`Rect::NOTHING`, whose arithmetic is infinite): the page draws its briefing
                // and its rows in a scroll area, so every row below the fold is one of these.
                // Only a positive, finite rectangle is a widget a reader can see and press.
                if !rect.rect.is_finite() || rect.rect.width() <= 0.0 || rect.rect.height() <= 0.0 {
                    return;
                }
                if colour == Color32::from_rgb(0x88, 0, 0x88)
                    && rect.rect.width() * rect.rect.height()
                        > panel.width() * panel.height() * 0.2
                {
                    canvases.push(rect.rect);
                }
            }
        });
    }
    (painted, canvases)
}

// -------------------------------------------------------------- the screens

/// One page at one size, painted the way the window paints it.
fn page_at(size: Vec2, page: Page, archive: &TempArchive, frames: usize) -> Painted {
    let ctx = context();
    let mut atlas = archive.atlas();
    atlas.set_page(page);
    let mut painted = Painted::default();
    for index in 0..frames {
        let output = frame(&ctx, size, index, &mut atlas, Vec::new());
        painted = Painted::read(&output);
    }
    painted
}

/// The rect a page's plate is given, measured from the frame and asserted to be the page's
/// only canvas.
fn one_canvas(size: Vec2, page: Page, archive: &TempArchive) -> Rect {
    let (_, mut found) = canvases(size, page, archive);
    assert_eq!(
        found.len(),
        1,
        "{} at {:.0}x{:.0} has {} canvases, not one: {found:?}",
        page.label(),
        size.x,
        size.y,
        found.len()
    );
    found.remove(0)
}

fn close(one: f32, other: f32) -> bool {
    (one - other).abs() <= TOL
}

/// The chrome above a page's canvas, measured from the frame: the lowest ink painted between
/// the content box's top and the canvas's top.
///
/// Ink, not layout: a label's galley is drawn a little above the bottom of the row it was
/// allocated, so the canvas starts a few pixels below this number. That distance is the page's
/// own spacing and is asserted against the theme's step, not assumed to be zero.
fn chrome_above(painted: &Painted, content: Rect, canvas: Rect) -> f32 {
    painted
        .text
        .iter()
        .map(|(_, rect)| rect)
        .filter(|rect| {
            // In the page's columns — the rail names every page too — and above the canvas.
            rect.min.x >= content.min.x - TOL
                && rect.max.x <= content.max.x + TOL
                && rect.max.y <= canvas.min.y + TOL
                && rect.max.y >= content.min.y - TOL
        })
        .map(|rect| rect.max.y)
        .fold(content.min.y, f32::max)
}

/// The strip a page leaves between the lowest ink of its chrome and the canvas.
///
/// The two pages that own a canvas spend a different step here and this is where each says
/// which: the graph page adds `theme::space::MD` under its header, and the Overview's map card
/// adds `theme::space::SM` under its title row. Measured against the ink, the strip is that
/// step less the descent of the last line — allowed to differ by [`INK_DESCENT`] and not more,
/// so a spacer added between the two shows up as a dead band.
const INK_DESCENT: f32 = 5.0;

/// The scroll bars painted across `area`: a thumb is a narrow, tall, filled rectangle, which
/// nothing else in this window draws. A canvas with one on it is a canvas a scroll area has
/// eaten — the window wraps neither page in one, and this is how that claim is measured.
fn thumbs_inside(painted: &Painted, area: Rect) -> Vec<Rect> {
    painted
        .filled
        .iter()
        .map(|(rect, _)| *rect)
        .filter(|rect| {
            rect.width() <= 12.0 && rect.height() >= 24.0 && rect.intersects(area.shrink(1.0))
        })
        .collect()
}

fn assert_strip(page: Page, size: Vec2, chrome: f32, canvas_top: f32, step: f32) {
    let strip = canvas_top - chrome;
    assert!(
        (strip - step).abs() <= INK_DESCENT,
        "{} at {:.0}x{:.0}: the canvas starts {:.1} px under the last ink of the header, and \
         the page spends {:.1} px there — {:.1} px of dead band",
        page.label(),
        size.x,
        size.y,
        strip,
        step,
        strip - step
    );
}

/// The measured table, printed by every run.
fn report(size: Vec2, page: Page, content: Rect, canvas: Rect, chrome: f32) {
    println!(
        "{} {:.0}x{:.0}: content {:.1?} · canvas x {:.1}..{:.1} y {:.1}..{:.1} \
         ({:.1}x{:.1}) · chrome above {:.1} · slack right {:.1} bottom {:.1}",
        page.label(),
        size.x,
        size.y,
        content.size(),
        canvas.min.x,
        canvas.max.x,
        canvas.min.y,
        canvas.max.y,
        canvas.width(),
        canvas.height(),
        chrome,
        content.max.x - canvas.max.x,
        content.max.y - canvas.max.y,
    );
}

// ------------------------------------------------------------------- the tests

/// Both canvases are handed the content box: the graph's plate and its relations column are
/// the only things under its heading, and the map's plate is the full inner width of its card,
/// keeping the world's own aspect. In every one of the three windows the page reaches the
/// bottom of the content box and nothing is painted outside it.
#[test]
fn the_map_and_the_graph_get_the_content_box() {
    let archive = TempArchive::build();
    for (width, height) in SIZES {
        let size = Vec2::new(width, height);

        // ---- the graph: a plate and a column, side by side, filling the box under the header
        let graph_painted = page_at(size, Page::Graph, &archive, 2);
        let content = content_box(size, &graph_painted, Page::Graph);
        let plate = one_canvas(size, Page::Graph, &archive);
        let chrome = chrome_above(&graph_painted, content, plate);
        report(size, Page::Graph, content, plate, chrome);

        assert!(
            close(plate.min.x, content.min.x),
            "graph at {width}x{height}: the plate starts at x {:.1}, the content box at {:.1}",
            plate.min.x,
            content.min.x
        );
        assert_strip(Page::Graph, size, chrome, plate.min.y, item_gap() + theme::space::MD);
        assert!(
            close(plate.max.y, content.max.y),
            "graph at {width}x{height}: the plate ends at y {:.1} and the content box at {:.1} \
             — {:.1} px left over at the bottom of the window",
            plate.max.y,
            content.max.y,
            content.max.y - plate.max.y
        );
        assert!(
            close(
                plate.width() + atlas_desktop::app::GRAPH_GAP + graph::PANEL_WIDTH,
                content.width()
            ),
            "graph at {width}x{height}: plate {:.1} + gap + panel {} is not the content width \
             {:.1}",
            plate.width(),
            graph::PANEL_WIDTH,
            content.width()
        );
        // What is painted past the content box, if anything: a name and a rectangle, so a
        // failure says which shape overflowed and by how much.
        let past: Vec<(&'static str, Rect)> = graph_painted
            .page_shapes(content)
            .filter(|(_, rect)| rect.max.y > content.max.y + 0.5)
            .collect();
        assert!(
            past.is_empty(),
            "graph at {width}x{height}: {} shapes are painted past the content box's bottom \
             ({:.1}): {:?}",
            past.len(),
            content.max.y,
            &past[..past.len().min(4)]
        );
        assert!(
            close(graph_painted.page_bottom(content), content.max.y),
            "graph at {width}x{height}: the lowest thing painted is at y {:.1}, the content box \
             ends at {:.1}",
            graph_painted.page_bottom(content),
            content.max.y
        );

        // ---- the map: the card's inner width, and the world's aspect, not the box's
        let map_painted = page_at(size, Page::Overview, &archive, 3);
        let map_content = content_box(size, &map_painted, Page::Overview);
        let map = one_canvas(size, Page::Overview, &archive);
        let map_chrome = chrome_above(&map_painted, map_content, map);
        assert_strip(Page::Overview, size, map_chrome, map.min.y, item_gap() + theme::space::SM);

        report(size, Page::Overview, map_content, map, map_chrome);

        assert!(
            close(map.min.x, map_content.min.x + CARD_CHROME),
            "overview at {width}x{height}: the plate starts at x {:.1}; the content box plus \
             the card's chrome ({CARD_CHROME}) is {:.1}",
            map.min.x,
            map_content.min.x + CARD_CHROME
        );
        // The card's right edge is the content box less its chrome — less the bar the page
        // draws down its own edge when the page scrolls, which it does as soon as the plate at
        // the world's aspect and the briefing's card do not both fit (see `views::overview`).
        let page_bar = context().global_style().spacing.scroll.allocated_width();
        assert!(
            map.max.x <= map_content.max.x - CARD_CHROME + TOL
                && map.max.x >= map_content.max.x - CARD_CHROME - page_bar - TOL,
            "overview at {width}x{height}: the plate ends at x {:.1}; the card's right edge is \
             {:.1} and the page's own bar is {page_bar:.1} wider than that",
            map.max.x,
            map_content.max.x - CARD_CHROME
        );
        // The height is the page's own answer for the room it was handed: the card's body runs
        // from the plate's top to the page's bottom less the card's bottom margin and border, and
        // `plate_height` is what turns that box into a plate. Measured against the page's own
        // function, so the test cannot disagree with the layout about arithmetic — only about
        // whether the box was measured correctly.
        let room = Vec2::new(map.width(), map_content.max.y - CARD_CHROME - map.min.y);
        let asked = overview::plate_height(room);
        println!(
            "overview {width}x{height}: room {:.1?} · plate {:.1} px tall, asked {:.1} · aspect \
             would be {:.1}",
            room,
            map.height(),
            asked,
            map.width() / overview::PLATE_ASPECT
        );
        assert!(
            (map.height() - asked).abs() <= TOL,
            "overview at {width}x{height}: the plate is {:.1} px tall and the page's own rule \
             asks for {:.1} px of the {:.1?} it was handed",
            map.height(),
            asked,
            room
        );
        assert!(
            map.height() <= map.width() / overview::PLATE_ASPECT + TOL,
            "overview at {width}x{height}: the plate is {:.1} px tall, and a {:.1} px plate at \
             the world's aspect {} is {:.1} px — the map would letterbox",
            map.height(),
            map.width(),
            overview::PLATE_ASPECT,
            map.width() / overview::PLATE_ASPECT
        );
        assert!(
            map.height() >= overview::MIN_PLATE,
            "overview at {width}x{height}: the plate is {:.1} px tall, under the documented \
             floor {}",
            map.height(),
            overview::MIN_PLATE
        );
        // The page fills the box it was given: when its blocks fit, the last card ends on the
        // box's foot; when they do not, the page scrolls and the bar it draws runs to the foot
        // (less the bar's own outer margin). Either way nothing is painted past it.
        let foot = map_painted.page_bottom(map_content);
        let bar_margin = context().global_style().spacing.scroll.bar_outer_margin;
        assert!(
            foot <= map_content.max.y + TOL && foot >= map_content.max.y - bar_margin - TOL,
            "overview at {width}x{height}: the lowest thing the page paints is at y {foot:.1}, so \
             the page {} the content box that ends at {:.1}",
            if foot < map_content.max.y - bar_margin - TOL {
                "stops short of"
            } else {
                "overflows"
            },
            map_content.max.y
        );
        assert!(
            map.min.y > map_content.min.y,
            "overview at {width}x{height}: the plate starts above the content box"
        );
        // A canvas is not inside a scroll area: no scroll bar may be painted over it.
        let thumbs = thumbs_inside(&map_painted, map);
        assert!(
            thumbs.is_empty(),
            "overview at {width}x{height}: {} scroll bars are painted on the plate {map:?}: {:?}",
            thumbs.len(),
            &thumbs[..thumbs.len().min(4)]
        );
    }
}

/// The graph's relations column does not sit on a node: the plate and the column are disjoint,
/// the gap between them is empty, and no node has any part inside the column.
#[test]
fn the_graph_panel_never_sits_on_a_node() {
    let archive = TempArchive::build();
    for (width, height) in SIZES {
        let size = Vec2::new(width, height);
        let painted = page_at(size, Page::Graph, &archive, 6);
        let content = content_box(size, &painted, Page::Graph);
        let plate = one_canvas(size, Page::Graph, &archive);
        let column = Rect::from_min_max(
            Pos2::new(plate.max.x + atlas_desktop::app::GRAPH_GAP, plate.min.y),
            content.max,
        );
        // The panel is rows of text and controls, not a card: its extent is the ink it paints
        // to the right of the plate, and that is what has to fit the column the plan promised.
        let ink = painted
            .page_shapes(content)
            .map(|(_, rect)| rect)
            .filter(|rect| rect.min.x >= plate.max.x + 1.0)
            .fold(Rect::NOTHING, |union, rect| union.union(rect));
        println!(
            "graph {width}x{height}: plate {:.1?} at ({:.1}, {:.1}) · column {:.1?} at \
             ({:.1}, {:.1}) · panel ink {:.1?} · nodes {}",
            plate.size(),
            plate.min.x,
            plate.min.y,
            column.size(),
            column.min.x,
            column.min.y,
            ink.size(),
            painted.dots_in(plate.expand(16.0)).len()
        );

        assert!(
            !painted.dots.is_empty(),
            "graph at {width}x{height}: no node was painted, so this test would pass by \
             measuring nothing"
        );
        assert!(
            close(column.width(), graph::PANEL_WIDTH),
            "graph at {width}x{height}: the relations column is {:.1} px wide, and the view \
             documents {}",
            column.width(),
            graph::PANEL_WIDTH
        );
        assert!(
            !plate.intersects(column),
            "graph at {width}x{height}: the plate {plate:?} and the column {column:?} overlap"
        );

        assert!(
            ink.width() > 0.0 && ink.height() > 0.0,
            "graph at {width}x{height}: nothing is painted right of the plate, so there is no \
             relations column to measure"
        );
        assert!(
            ink.min.x >= column.min.x - TOL
                && ink.max.x <= column.max.x + TOL
                && ink.min.y >= column.min.y - TOL
                && ink.max.y <= column.max.y + TOL,
            "graph at {width}x{height}: the relations panel paints {ink:?}, outside the column \
             {column:?} the page promised it (a panel drawn past the window's edge is the defect \
             this measures)"
        );
        let gap = Rect::from_min_max(
            Pos2::new(plate.max.x, content.min.y),
            Pos2::new(column.min.x, content.max.y),
        );
        for seen in &painted.shapes {
            let drawn = Painted::painted_rect(seen);
            if drawn == Rect::NOTHING {
                continue;
            }
            // The page's own background is painted *under* both halves of the page and covers
            // the gap by existing: a rectangle that contains the plate is furniture, not paint
            // that landed between the plate and the column.
            if drawn.contains_rect(plate) {
                continue;
            }
            assert!(
                !drawn.intersects(gap.shrink(0.5)),
                "graph at {width}x{height}: a {} {drawn:?} is painted in the {:.1} px gap \
                 between the plate and the column ({gap:?})",
                seen.kind,
                gap.width()
            );
        }

        // An empty panel is a hint, one line: the state that could push a row past the column's
        // edge is the panel with an entity open, so select one and measure that too.
        let mut screen = Screen::new(size);
        let mut atlas = archive.atlas();
        atlas.set_page(Page::Graph);
        // The plate's layout cools asymptotically from a cold start: the graph takes the same
        // 400-frame ceiling the camera round trip below allows it.
        let settled = screen.settle(&mut atlas, 400);
        let node = settled
            .dots_in(plate)
            .into_iter()
            .min_by(|a, b| {
                (*a - plate.center())
                    .length()
                    .partial_cmp(&(*b - plate.center()).length())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("the graph paints no node to select");
        screen.next(&mut atlas, press(node, true));
        screen.next(&mut atlas, press(node, false));
        // Selecting re-seeds the plate's cooling, which is why the plate is not asked to be still
        // here: what this measures is the panel, whose rows are drawn the frame the click lands.
        let mut listed = Painted::default();
        for _ in 0..24 {
            listed = screen.next(&mut atlas, Vec::new());
        }
        let opened = listed
            .page_shapes(content)
            .map(|(_, rect)| rect)
            .filter(|rect| rect.min.x >= plate.max.x + 1.0)
            .fold(Rect::NOTHING, |union, rect| union.union(rect));
        println!(
            "graph {width}x{height}: panel with an entity open {:.1?} at ({:.1}, {:.1}) · \
             the empty panel was {:.1?}",
            opened.size(),
            opened.min.x,
            opened.min.y,
            ink.size()
        );
        assert!(
            opened.height() > ink.height() + TOL,
            "graph at {width}x{height}: selecting the node at {node:?} grew the relations panel \
             from {:.1} px to {:.1} px tall, so nothing was measured here",
            ink.height(),
            opened.height()
        );
        assert!(
            opened.min.x >= column.min.x - TOL
                && opened.max.x <= column.max.x + TOL
                && opened.min.y >= column.min.y - TOL
                && opened.max.y <= column.max.y + TOL,
            "graph at {width}x{height}: with an entity open the panel paints {opened:?}, outside \
             the column {column:?} the page promised it"
        );
        for seen in &listed.shapes {
            let drawn = Painted::painted_rect(seen);
            if drawn == Rect::NOTHING || drawn.contains_rect(plate) {
                continue;
            }
            assert!(
                !drawn.intersects(gap.shrink(0.5)),
                "graph at {width}x{height}: with an entity open a {} {drawn:?} is painted in the \
                 gap between the plate and the column",
                seen.kind
            );
        }

        let nodes = painted.dots_in(plate.expand(TOL.max(16.0)).union(column));
        let inside: Vec<Pos2> = nodes.iter().copied().filter(|at| column.contains(*at)).collect();
        assert!(
            inside.is_empty(),
            "graph at {width}x{height}: {} of {} nodes are inside the relations column {column:?}: \
             {:?}",
            inside.len(),
            nodes.len(),
            &inside[..inside.len().min(4)]
        );
        // The nodes are on the plate, not merely outside the column.
        let off: Vec<Pos2> = nodes
            .iter()
            .copied()
            .filter(|at| !plate.expand(TOL).contains(*at))
            .collect();
        assert!(
            off.is_empty(),
            "graph at {width}x{height}: {} of {} nodes are off the plate {plate:?}: {:?}",
            off.len(),
            nodes.len(),
            &off[..off.len().min(4)]
        );
    }
}

/// The map's zoom, written on the plate's caption, at every size: the fit is ×1.0 on the
/// opening frame, because a plate at the world's aspect needs no zoom to hold the world.
#[test]
fn the_map_opens_at_its_own_fit() {
    let archive = TempArchive::build();
    for (width, height) in SIZES {
        let size = Vec2::new(width, height);
        let painted = page_at(size, Page::Overview, &archive, 3);
        let caption = painted
            .text
            .iter()
            .find(|(text, _)| text.starts_with("zoom ×"))
            .map(|(text, _)| text.clone())
            .unwrap_or_else(|| {
                panic!(
                    "overview at {width}x{height} painted no zoom caption; it painted:\n{}",
                    painted.words()
                )
            });
        println!("overview {width}x{height}: {caption}");
        assert!(
            caption.starts_with("zoom ×1.0"),
            "overview at {width}x{height}: the plate opens at {caption:?}, not at the fit"
        );
    }
}

// ----------------------------------------------------------------- the cameras

/// One screen at one size, with a clock that never goes backwards: egui reads the time for its
/// own animations, and a frame stamped earlier than the one before it is a frame out of order.
struct Screen {
    ctx: egui::Context,
    size: Vec2,
    clock: usize,
}

impl Screen {
    fn new(size: Vec2) -> Screen {
        Screen {
            ctx: context(),
            size,
            clock: 0,
        }
    }

    /// One frame, read.
    fn next(&mut self, atlas: &mut Atlas, events: Vec<egui::Event>) -> Painted {
        let output = frame(&self.ctx, self.size, self.clock, atlas, events);
        self.clock += 1;
        Painted::read(&output)
    }

    /// Draw until the picture stops moving, and return the frame it stopped on.
    ///
    /// Both canvases settle over frames — the map's camera eases toward its target and the
    /// graph's layout cools — so a measurement taken on the frame a gesture arrived on would be
    /// a measurement of a transition. The cap is a failure: a page that never settles is a page
    /// that repaints forever.
    fn settle(&mut self, atlas: &mut Atlas, cap: usize) -> Painted {
        let mut previous: Vec<Pos2> = Vec::new();
        let mut previous_caption: Option<String> = None;
        let mut painted = Painted::default();
        for frame_index in 0..cap {
            painted = self.next(atlas, Vec::new());
            let mut dots = painted.dots.clone();
            dots.sort_by(|a, b| {
                a.x.partial_cmp(&b.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
            });
            let mut still = dots.len() == previous.len()
                && dots
                    .iter()
                    .zip(&previous)
                    .all(|(now, before)| (*now - *before).length() <= 0.05);
            // The map's camera writes its zoom on the plate: a map still easing toward a target
            // holds the same dots when the pins have left the plate, and this is what says so.
            still &= painted.zoom_caption().map(str::to_owned) == previous_caption;
            previous_caption = painted.zoom_caption().map(str::to_owned);
            previous = dots;
            if still && frame_index > 0 {
                return painted;
            }
        }
        panic!(
            "the page did not settle in {cap} frames: the last dot box was {:?}",
            painted.dot_spread(Rect::EVERYTHING)
        );
    }
}

/// A wheel of `points` at `at`: the pointer moves there first, so the plate under it is the one
/// that takes the scroll.
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

/// A press or a release at `at`, with the pointer move that puts it there first.
fn press(at: Pos2, down: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// The zoom the map paints on its own caption: `zoom ×1.4 · 6 places · …`.
fn zoom_of(painted: &Painted) -> f32 {
    let caption = painted
        .text
        .iter()
        .find(|(text, _)| text.starts_with("zoom ×"))
        .map(|(text, _)| text.clone())
        .unwrap_or_else(|| {
            panic!("the plate painted no zoom caption; it painted:\n{}", painted.words())
        });
    let number = caption
        .trim_start_matches("zoom ×")
        .split(' ')
        .next()
        .unwrap_or("");
    number
        .parse::<f32>()
        .unwrap_or_else(|_| panic!("the plate's caption {caption:?} does not start with a zoom"))
}

/// The map's camera is the map's: a wheel over the plate zooms it about the pointer, and a walk
/// to the Graph page and back leaves the very same zoom and the very same picture.
///
/// The zoom is read from the caption the plate writes on itself (`zoom ×1.4 · 6 places · …`)
/// and the pan from the box the pins occupy on the plate: a zoom multiplies that box, a pan
/// slides it, and a camera rebuilt on page entry restores it to the opening one.
#[test]
fn the_map_keeps_its_camera_across_a_page_switch() {
    let archive = TempArchive::build();
    let size = Vec2::new(1280.0, 800.0);
    let mut screen = Screen::new(size);
    let mut atlas = archive.atlas();

    atlas.set_page(Page::Overview);
    let opening = screen.settle(&mut atlas, 60);
    let plate = one_canvas(size, Page::Overview, &archive);
    let zoom_opening = zoom_of(&opening);
    screen.next(&mut atlas, wheel(plate.center(), 150.0));
    let zoomed = screen.settle(&mut atlas, 120);
    let zoom_after = zoom_of(&zoomed);
    let middle_after = zoomed.dot_middle(plate);
    println!(
        "map {size:?}: zoom ×{zoom_opening} -> ×{zoom_after} · pin middle {:?} -> {middle_after:?} \
         on {:.1?}",
        opening.dot_middle(plate),
        plate.size()
    );
    assert!(
        zoom_after > zoom_opening + 0.1,
        "a wheel of 150 points over the plate moved the map from ×{zoom_opening} to ×{zoom_after}, \
         so there is nothing for the round trip below to preserve"
    );

    atlas.set_page(Page::Graph);
    screen.next(&mut atlas, Vec::new());
    atlas.set_page(Page::Overview);
    let back = screen.settle(&mut atlas, 60);
    assert_eq!(
        zoom_of(&back),
        zoom_after,
        "the map was at ×{zoom_after} when the reader left it and at ×{} when they came back",
        zoom_of(&back)
    );
    assert!(
        (back.dot_middle(plate) - middle_after).length() <= 1.0,
        "the map's picture moved {:.1} px across a page switch: {middle_after:?} -> {:?}",
        (back.dot_middle(plate) - middle_after).length(),
        back.dot_middle(plate)
    );
}

/// How far apart two pictures of the same dots are: the largest distance between their centres,
/// or `None` when they do not hold the same number of dots.
fn picture_gap(one: &[Pos2], other: &[Pos2]) -> Option<f32> {
    if one.len() != other.len() {
        return None;
    }
    one.iter()
        .zip(other)
        .map(|(left, right)| (*left - *right).length())
        .fold(Some(0.0_f32), |worst, distance| {
            Some(worst.unwrap_or(0.0).max(distance))
        })
}

/// The graph's camera **does** survive the same trip, which is the opposite of what this test
/// measured when it was written — the old doc said so itself: "the day `app.rs` stops refilling a
/// graph whose entities and relations did not change, the first assertion fails and says to
/// invert it". That day came: `views::graph::fill` now keeps the layout, the camera, the selection
/// and the open relation when the set it is handed is the set it already holds, and the shell no
/// longer re-reads the graph for a page-only change.
///
/// The reader's complaint this pins: zoom in, step to Overview, come back, and the picture is at
/// the top of the canvas again.
///
/// What is asserted, in the order the old version asserted the opposite: the picture that comes
/// back is the gesture's picture, not the fresh window's (so the round trip kept the camera), and
/// the gesture's picture is still unlike the fresh one (so the test is measuring something).
#[test]
fn a_page_switch_keeps_the_graphs_camera() {
    let archive = TempArchive::build();
    let size = Vec2::new(1280.0, 800.0);
    let mut screen = Screen::new(size);
    let mut atlas = archive.atlas();

    // The map first, zoomed: it is the control for "the switch touched only the graph".
    atlas.set_page(Page::Overview);
    screen.settle(&mut atlas, 60);
    let map_plate = one_canvas(size, Page::Overview, &archive);
    screen.next(&mut atlas, wheel(map_plate.center(), 150.0));
    let map_zoom = zoom_of(&screen.settle(&mut atlas, 120));
    assert!(map_zoom > 1.1, "the map did not zoom: ×{map_zoom}");

    // The graph: settle it, then zoom it and drag its plate.
    atlas.set_page(Page::Graph);
    let plate = one_canvas(size, Page::Graph, &archive);
    let opening = screen.settle(&mut atlas, 400).dot_set(plate);
    // 60 frames, not 4: the page's own bar eases in over the first frames after a switch, and a
    // dot that is still moving is not a dot this test may call settled.
    let middle_opening = screen.settle(&mut atlas, 60).dot_middle(plate);
    screen.next(&mut atlas, wheel(plate.center(), 150.0));
    let from = plate.center() - Vec2::new(60.0, 0.0);
    let to = from + Vec2::new(80.0, 40.0);
    screen.next(&mut atlas, press(from, true));
    screen.next(&mut atlas, vec![egui::Event::PointerMoved(to)]);
    screen.next(&mut atlas, press(to, false));
    let moved = screen.settle(&mut atlas, 400);
    let moved_dots = moved.dot_set(plate);

    // The same archive on a window that never touched the graph: what a refill paints.
    let mut fresh = archive.atlas();
    fresh.set_page(Page::Graph);
    let mut other = Screen::new(size);
    let fresh_dots = other.settle(&mut fresh, 400).dot_set(plate);
    println!(
        "graph {size:?}: {} dots on {:.1?} · gesture moves the middle {:.1} px · gesture vs a \
         fresh window {:?} px",
        opening.len(),
        plate.size(),
        (moved.dot_middle(plate) - middle_opening).length(),
        picture_gap(&moved_dots, &fresh_dots)
    );

    let gesture_gap = picture_gap(&moved_dots, &fresh_dots).unwrap_or_else(|| {
        panic!(
            "the gesture left {} dots and a fresh window paints {}: the graph's camera is not a \
             picture of the same picture any more, so this test cannot measure a reset",
            moved_dots.len(),
            fresh_dots.len()
        )
    });
    assert!(
        (moved.dot_middle(plate) - middle_opening).length() > 10.0 && gesture_gap > 10.0,
        "a wheel of 150 points and an 80 × 40 px drag moved the graph's picture {:.1} px (middle \
         {middle_opening:?} -> {:?}), so there is nothing for the round trip below to measure",
        gesture_gap,
        moved.dot_middle(plate)
    );

    // Away and back: the plate is where it was, and the picture is not.
    atlas.set_page(Page::Overview);
    for _ in 0..3 {
        screen.next(&mut atlas, Vec::new());
    }
    assert_eq!(
        zoom_of(&screen.settle(&mut atlas, 120)),
        map_zoom,
        "the graph's gesture or the walk to Overview moved the map's zoom"
    );
    atlas.set_page(Page::Graph);
    let returned = screen.settle(&mut atlas, 400);
    let returned_dots = returned.dot_set(plate);
    let gap = picture_gap(&returned_dots, &fresh_dots);
    println!(
        "graph {size:?}: after Overview and back, {} dots, {gap:?} px from the fresh window's \
         picture and {:?} px from the gesture's",
        returned_dots.len(),
        picture_gap(&returned_dots, &moved_dots)
    );
    assert!(
        picture_gap(&returned_dots, &moved_dots).is_some_and(|kept| kept <= 1.0),
        "the graph came back with {} dots at {:?} px from the picture the reader left — so the \
         page switch threw the camera away again",
        returned_dots.len(),
        picture_gap(&returned_dots, &moved_dots)
    );
    assert!(
        gap.is_some_and(|gap| gap > 10.0),
        "the picture that came back is {gap:?} px from a window that never touched the graph, so \
         no gesture was kept and this test measures nothing"
    );
}

