//! What the window actually paints, measured off the frame instead of looked at.
//!
//! The house rules are absolute and they are about pixels, not intentions: strictly
//! monochrome, no gradient, no glow, one corner radius, one spacing scale, one type ladder.
//! A view can satisfy all of them in its source and still break one in the paint — a literal
//! `Color32::from_rgb(7,7,7)` on a plate, a `FontId::proportional(10.5)`, a 6 px padding — so
//! this file reads the shapes a frame hands a backend and asks the six questions directly:
//!
//! 1. **Palette and grey.** Every fill and stroke that paints something is a value from
//!    [`theme::ALL`], its three channels are equal, and it is opaque. The theme documents no
//!    partial alpha step, so a translucent paint is reported rather than excused.
//! 2. **Flat.** No multi-stop mesh, no band, no textured rect or mesh, no blurred rect, no
//!    stroke coloured by position, no paint callback: those are how a gradient, a
//!    gradient-as-texture, a glow and a shadow arrive in `epaint`. A mesh of one colour is the
//!    ordinary way a filled polygon is painted and is flat paint.
//! 3. **Contrast.** Every line of text is measured against the surface it lands on — the last
//!    fill painted underneath it, or the page if there is none — at the 4.5:1 WCAG asks of
//!    body text. The worst pair per screen is printed.
//! 4. **Radius.** Every painted rectangle's four corners are 0 or [`theme::RADIUS`], and the
//!    radius in the theme is the one this file pins, so raising it in the theme is a change
//!    here and not a silent redesign.
//! 5. **Spacing.** The spacing a surface decides is its padding, and a painted frame exposes it
//!    as the left inset of the column of text it holds — a whole number of
//!    [`theme::space::UNIT`]s, within 1 px for the hairline between two rectangles. A gap
//!    *between* two blocks is not measured: it belongs to neither rectangle, and the distance
//!    between two lines is line height as much as it is spacing.
//! 6. **Type.** Every line of text is set in a size on [`theme::LADDER`], in a (size, face) pair
//!    one of [`theme::role::ALL`] actually produces.
//!
//! # How a screen is painted
//!
//! The six pages are the real thing: [`Atlas::with_archive`] over a temporary archive and
//! [`Atlas::draw`] on the root `Ui` of `egui::Context::run_ui` — the call `eframe` makes, with a
//! screen rectangle and a clock instead of a GPU surface. `SETTLE` frames are run and the last is
//! read: egui lays a column out once and settles, and the Graph page's heat decays over frames,
//! so the first frame is not what a reader sees. The terms screen is the same window with
//! [`Atlas::with_terms`] over a queue that has never been written. **Settings** is the one
//! screen the window cannot reach: [`Page`] has six variants and no Settings, and
//! `views::settings::show` has no caller in `src/`, so the block is drawn directly, inside the
//! same central-panel frame the app draws its pages in. That is reported, not fixed here.
//!
//! Two sizes are used — 1280×800 and the smallest window the layout must fit — because a
//! tighter column wraps text and wrapping changes which surface a line lands on.
//!
//! # What is deliberately not a finding
//!
//! Two kinds of shape carry no ink and are skipped before the rules see them: a galley whose
//! text is empty or whitespace (an empty line is laid out with a NaN rectangle and prints
//! nothing), and a shape whose rectangle is pruned entirely by its clip. Both are invisible in
//! the window, so auditing them would be auditing the tessellator. Everything visible is read.
//!
//! # What this file does not do
//!
//! It fixes nothing. A violation is a measurement to act on, and the view that owns it is not
//! this file's to edit. Run it with the table printed:
//!
//! ```text
//! cargo test --offline -p atlas-desktop --test design_audit -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::epaint::{ClippedShape, Shape};
use egui::{vec2, Color32, FontFamily, Rect, Vec2};

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::theme;
use atlas_desktop::views::settings::{self, Engine};

/// The windows the audit is taken at: the one `main.rs` opens, and the smallest the layout
/// must still fit.
const SIZES: [(f32, f32); 2] = [(1280.0, 800.0), (900.0, 600.0)];

/// WCAG 2.2 asks 4.5:1 of body text.
const CONTRAST_FLOOR: f64 = 4.5;

/// A rectangle's corners may be square or on [`theme::RADIUS`]; egui's 3 px and 6 px defaults
/// are the two the theme lowers, and a third value would be a redesign.
const RADII: [u8; 2] = [0, theme::RADIUS];

/// The page frame `app.rs` draws its pages in (`app.rs:533-534`), reproduced for the one
/// screen the app has no page for.
const PAGE_MARGIN: (i8, i8) = (20, 16);

// ------------------------------------------------------------------------------- the screens

/// One screen of the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Screen {
    Overview,
    Articles,
    Topics,
    Monitors,
    Graph,
    Chat,
    /// Drawn straight from `views::settings`, because the rail has no way to it.
    Settings,
    /// The terms, which is the whole window until they are accepted.
    Gate,
}

/// Every screen, in the rail's order and then the two that are not in it.
const SCREENS: [Screen; 8] = [
    Screen::Overview,
    Screen::Articles,
    Screen::Topics,
    Screen::Monitors,
    Screen::Graph,
    Screen::Chat,
    Screen::Settings,
    Screen::Gate,
];

impl Screen {
    /// How the screen is named in the table and in a failure message.
    fn name(self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Articles => "Articles",
            Screen::Topics => "Topics",
            Screen::Monitors => "Monitors",
            Screen::Graph => "Graph",
            Screen::Chat => "Chat",
            Screen::Settings => "Settings",
            Screen::Gate => "Licence gate",
        }
    }

    /// The page the rail shows this screen on, or `None` for the two the rail has no entry
    /// for.
    fn page(self) -> Option<Page> {
        match self {
            Screen::Overview => Some(Page::Overview),
            Screen::Articles => Some(Page::Articles),
            Screen::Topics => Some(Page::Topics),
            Screen::Monitors => Some(Page::Monitors),
            Screen::Graph => Some(Page::Graph),
            Screen::Chat => Some(Page::Chat),
            Screen::Settings | Screen::Gate => None,
        }
    }
}

// ------------------------------------------------------------------------------- the archive

/// A `data/atlas.db` of our own in a temp directory, removed when the test ends. The live
/// archive is never opened: an audit that read the reader's history would measure a different
/// window on every machine.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
    /// A second path in the same directory that is never created: the request queue. A queue
    /// with no row in it is a machine that has not accepted the terms yet.
    queue: PathBuf,
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
            "atlas-design-audit-{}-{}-{}",
            std::process::id(),
            nanos,
            NEXT_ARCHIVE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("atlas.db");
        let queue = dir.join("requests.db");
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
        // Six stories spread across the world, so a pin, a node and a row are never all in the
        // same corner of a plate.
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
        for (id, name, keywords, active, hits) in [
            (1, "Harbour labour", "harbour,crane,strike", 1, 34),
            (2, "Alpine freight", "rail,freight,alpine", 1, 12),
            (3, "Chip exports", "chip,export,semiconductor", 0, 7),
        ] {
            conn.execute(
                "INSERT INTO topics(id, name, keywords, active, hits, last_hit) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, name, keywords, active, hits, recent],
            )
            .expect("a topic");
        }
        for (id, name, query, active, cadence, hits) in [
            (1, "Harbour strikes", "harbour strike", 1, 60, 9),
            (2, "Alpine rail", "alpine rail freight", 1, 360, 3),
            (3, "Chip rules", "chip export rules", 0, 1440, 1),
        ] {
            conn.execute(
                "INSERT INTO monitors(id, name, query, active, cadence_minutes, last_run, hits) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![id, name, query, active, cadence, recent, hits],
            )
            .expect("a monitor");
        }
        drop(conn);
        TempArchive { dir, file, queue }
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

// ------------------------------------------------------------------------- driving a frame

/// The window as a release build paints it: the house style, and **none** of egui's debug ink.
///
/// Two of egui's `DebugOptions` paint in a debug build and would be audited as if the app had
/// painted them: `show_unaligned` defaults to `cfg!(debug_assertions)` and draws every
/// unaligned rectangle edge in `Color32::ORANGE` with the word "Unaligned"
/// (`egui-0.37.0/src/ui.rs:3042`), and `show_widget_hits` outlines response rects.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.all_styles_mut(|style| {
        style.debug.debug_on_hover = false;
        style.debug.show_widget_hits = false;
        style.debug.show_interactive_widgets = false;
        style.debug.show_unaligned = false;
        style.debug.show_focused_widget = false;
        style.debug.show_expand_width = false;
        style.debug.show_expand_height = false;
        style.debug.show_resize = false;
    });
    ctx
}

fn raw_input(size: Vec2, index: usize) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
        time: Some(index as f64 / 60.0),
        ..Default::default()
    }
}

/// One frame of the real window, with the texture deltas dropped the way a backend uploads
/// them (`epaint-0.37.0/src/textures.rs:337` panics on `Drop` otherwise).
fn frame(ctx: &egui::Context, size: Vec2, index: usize, atlas: &mut Atlas) -> egui::FullOutput {
    let output = ctx.run_ui(raw_input(size, index), |ui| atlas.draw(ui));
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

/// One frame of the Settings block, inside the frame the app draws its pages in.
fn settings_frame(
    ctx: &egui::Context,
    size: Vec2,
    index: usize,
    engine: &Engine,
    queue: &std::path::Path,
) -> egui::FullOutput {
    let output = ctx.run_ui(raw_input(size, index), |ui| {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE.fill(theme::PAGE).inner_margin(egui::Margin::symmetric(
                    PAGE_MARGIN.0,
                    PAGE_MARGIN.1,
                )),
            )
            .show(ui, |ui| settings::show_with_queue(ui, engine, queue));
    });
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

/// How many frames are run before the one that is read. egui settles a layout on its second
/// frame, and the Graph page's heat decays over about two seconds of them.
const SETTLE: usize = 4;

impl TempArchive {
    /// Paint one screen at one size, and read the last frame.
    fn capture(&self, screen: Screen, size: Vec2) -> Capture {
        let ctx = context();
        let mut last = None;
        match screen.page() {
            Some(page) => {
                let mut atlas = Atlas::with_archive(self.file.clone()).with_queue(self.queue.clone());
                atlas.set_page(page);
                for index in 0..=SETTLE {
                    last = Some(frame(&ctx, size, index, &mut atlas));
                }
            }
            None if screen == Screen::Gate => {
                let mut atlas = Atlas::with_archive(self.file.clone())
                    .with_queue(self.queue.clone())
                    .with_terms();
                assert!(atlas.gate_is_up(), "the terms screen is not up");
                for index in 0..=SETTLE {
                    last = Some(frame(&ctx, size, index, &mut atlas));
                }
            }
            None => {
                let engine = Engine::in_use(None, settings::BUILT_IN);
                for index in 0..=SETTLE {
                    last = Some(settings_frame(&ctx, size, index, &engine, &self.queue));
                }
            }
        }
        let output = last.expect("a frame");
        Capture::read(screen.name(), size, &output)
    }
}

// ------------------------------------------------------------------------- reading a frame

/// One paint: a fill or a stroke, with what painted it and what it painted.
#[derive(Clone, Copy)]
struct Paint {
    /// The shape that carried it: `rect fill`, `rect stroke`, `text`, …
    through: &'static str,
    colour: Color32,
    rect: Rect,
}

/// A filled area, in paint order. The last one under a line of text is its surface.
#[derive(Clone, Copy)]
struct Fill {
    index: usize,
    rect: Rect,
    colour: Color32,
}

/// One line of text as a backend sees it: where it went, in what size, in what face, in what
/// colour.
struct Ink {
    index: usize,
    text: String,
    rect: Rect,
    colour: Color32,
    size: f32,
    face: Face,
}

/// The two faces that ship ([`theme::Weight`]), and anything else, which is a third face the
/// tree does not have.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Face {
    Text,
    Number,
    Other,
}

impl Face {
    fn of(family: &FontFamily) -> Face {
        match family {
            FontFamily::Proportional => Face::Text,
            FontFamily::Monospace => Face::Number,
            FontFamily::Name(_) => Face::Other,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Face::Text => "text face",
            Face::Number => "number face",
            Face::Other => "a named face",
        }
    }
}

/// A corner radius, as painted.
#[derive(Clone, Copy)]
struct Corner {
    rect: Rect,
    radius: u8,
}

/// A shape that is not a flat fill, a flat stroke or text: the ways a gradient, a texture, a
/// glow or a shadow reaches `epaint`.
#[derive(Clone)]
struct Foreign {
    kind: String,
    rect: Rect,
}

/// One frame, counted: every colour it painted, every surface, every line of text, every
/// corner, and every shape the six rules cannot read as flat paint.
#[derive(Default)]
struct Capture {
    screen: &'static str,
    size: Vec2,
    shapes: usize,
    paints: Vec<Paint>,
    fills: Vec<Fill>,
    inks: Vec<Ink>,
    corners: Vec<Corner>,
    foreign: Vec<Foreign>,
}

impl Capture {
    fn read(screen: &'static str, size: Vec2, output: &egui::FullOutput) -> Capture {
        let mut capture = Capture {
            screen,
            size,
            ..Default::default()
        };
        let mut index = 0;
        for ClippedShape {
            clip_rect, shape, ..
        } in &output.shapes
        {
            walk(&mut capture, shape, *clip_rect, &mut index);
        }
        capture
    }

    /// A name for the screen and the size, for every message this file writes.
    fn at(&self) -> String {
        format!(
            "{} {}×{}",
            self.screen,
            self.size.x.round() as i64,
            self.size.y.round() as i64
        )
    }

    /// How many distinct colours this frame painted.
    fn colour_count(&self) -> usize {
        let mut seen: Vec<Color32> = Vec::new();
        for paint in &self.paints {
            if !seen.contains(&paint.colour) {
                seen.push(paint.colour);
            }
        }
        seen.len()
    }
}

/// Whether a shape is on screen at all: its rectangle, cut by its clip, is not empty. A pruned
/// shape is not painted and is not audited.
///
/// The finiteness check comes first, and it is not cosmetic: `Rect::NOTHING` is
/// `[+∞, -∞]` and `PartialEq` on a rectangle panics on a NaN (`emath-0.37.0/src/rect.rs:22`),
/// so a shape that paints at a NaN has to be caught by asking `is_finite` and never by
/// comparing.
fn live(rect: Rect, clip: Rect) -> bool {
    if !rect.is_finite() {
        return false;
    }
    let cut = rect.intersect(clip);
    cut.width() > 0.0 && cut.height() > 0.0
}

/// Whether a rectangle has a coordinate that is not a number: a shape painted at a position
/// that is not a place.
fn has_nan(rect: Rect) -> bool {
    rect.min.x.is_nan() || rect.min.y.is_nan() || rect.max.x.is_nan() || rect.max.y.is_nan()
}

fn push_paint(capture: &mut Capture, through: &'static str, colour: Color32, rect: Rect) {
    // `TRANSPARENT` is what a shape says when it has no fill or no stroke: it paints nothing,
    // so it is not a colour the palette has to name.
    if colour.a() == 0 {
        return;
    }
    capture.paints.push(Paint {
        through,
        colour,
        rect,
    });
}

/// What a shape is, for a message about one.
fn kind_of(shape: &Shape) -> &'static str {
    match shape {
        Shape::Noop => "nothing",
        Shape::Vec(_) => "group",
        Shape::Circle(_) => "circle",
        Shape::Ellipse(_) => "ellipse",
        Shape::LineSegment { .. } => "line",
        Shape::Path(_) => "path",
        Shape::Band(_) => "band",
        Shape::Rect(_) => "rect",
        Shape::Text(_) => "text",
        Shape::Mesh(_) => "mesh",
        Shape::QuadraticBezier(_) => "curve",
        Shape::CubicBezier(_) => "curve",
        Shape::Callback(_) => "callback",
    }
}

/// A path's stroke is either one colour or a function of position — which is a gradient with a
/// closure for a palette, so it is named rather than sampled.
fn push_path_stroke(
    capture: &mut Capture,
    through: &'static str,
    colour: &egui::epaint::ColorMode,
    rect: Rect,
) {
    match colour {
        egui::epaint::ColorMode::Solid(colour) => push_paint(capture, through, *colour, rect),
        egui::epaint::ColorMode::UV(_) => capture.foreign.push(Foreign {
            kind: "a stroke coloured by position".to_owned(),
            rect,
        }),
    }
}

/// Walk one shape and everything under it, in paint order.
fn walk(capture: &mut Capture, shape: &Shape, clip: Rect, order: &mut usize) {
    let index = *order;
    *order += 1;
    // The index is what tells a text apart from the fills painted before it: see `surface`.
    capture.shapes += 1;
    // A galley with no glyphs in it paints nothing, whether it is an empty line or a space
    // between two words: epaint still hands it a rectangle, and for an empty line that
    // rectangle can be a NaN. It is not a paint, so it is not a finding.
    if let Shape::Text(text) = shape {
        if text.galley.text().trim().is_empty() {
            return;
        }
    }
    let rect = shape.visual_bounding_rect();
    // A paint callback paints outside epaint, so it has no rectangle to check and no colour to
    // read: it is named as foreign whether or not it is inside its clip.
    if let Shape::Callback(_) = shape {
        capture.foreign.push(Foreign {
            kind: "a paint callback".to_owned(),
            rect: Rect::NOTHING,
        });
        return;
    }
    if has_nan(rect) {
        let detail = match shape {
            Shape::Text(text) => format!(" (the line {:?})", sample(text.galley.text())),
            other => format!(" ({})", kind_of(other)),
        };
        capture.foreign.push(Foreign {
            kind: format!("a {} painted at a NaN{detail}", kind_of(shape)),
            rect: Rect::ZERO,
        });
        return;
    }
    if !live(rect, clip) {
        return;
    }
    match shape {
        Shape::Noop => {}
        Shape::Vec(shapes) => {
            for child in shapes {
                walk(capture, child, clip, order);
            }
        }
        Shape::Circle(circle) => {
            push_paint(capture, "circle fill", circle.fill, rect);
            if circle.stroke.width > 0.0 {
                push_paint(capture, "circle stroke", circle.stroke.color, rect);
            }
        }
        Shape::Ellipse(ellipse) => {
            push_paint(capture, "ellipse fill", ellipse.fill, rect);
            if ellipse.stroke.width > 0.0 {
                push_paint(capture, "ellipse stroke", ellipse.stroke.color, rect);
            }
        }
        Shape::LineSegment { stroke, .. } => {
            if stroke.width > 0.0 {
                push_paint(capture, "line", stroke.color, rect);
            }
        }
        Shape::Path(path) => {
            push_paint(capture, "path fill", path.fill, rect);
            if path.stroke.width > 0.0 {
                push_path_stroke(capture, "path stroke", &path.stroke.color, rect);
            }
        }
        Shape::Band(band) => {
            push_paint(capture, "band fill", band.fill, rect);
            capture.foreign.push(Foreign {
                kind: "a band".to_owned(),
                rect,
            });
        }
        Shape::Rect(rect_shape) => {
            let fill = rect_shape.fill;
            if fill.a() > 0 {
                push_paint(capture, "rect fill", fill, rect_shape.rect);
                capture.fills.push(Fill {
                    index,
                    rect: rect_shape.rect,
                    colour: fill,
                });
            }
            if rect_shape.stroke.width > 0.0 {
                push_paint(capture, "rect stroke", rect_shape.stroke.color, rect_shape.rect);
            }
            let corners = rect_shape.corner_radius;
            capture.corners.push(Corner {
                rect: rect_shape.rect,
                radius: corners.nw.max(corners.ne).max(corners.sw).max(corners.se),
            });
            if rect_shape.blur_width > 0.0 {
                capture.foreign.push(Foreign {
                    kind: "a blurred rect".to_owned(),
                    rect,
                });
            }
            if let Some(brush) = &rect_shape.brush {
                let textured = brush.fill_texture_id != egui::TextureId::default()
                    && brush.uv != Rect::ZERO;
                if textured {
                    capture.foreign.push(Foreign {
                        kind: "a textured rect".to_owned(),
                        rect,
                    });
                }
            }
            if rect_shape.angle != 0.0 {
                capture.foreign.push(Foreign {
                    kind: "a rotated rect".to_owned(),
                    rect,
                });
            }
        }
        Shape::Text(text_shape) => {
            if text_shape.underline.width > 0.0 {
                push_paint(capture, "underline", text_shape.underline.color, rect);
            }
            let fallback = text_shape.fallback_color;
            let words = text_shape.galley.job.text.clone();
            for section in &text_shape.galley.job.sections {
                let format = &section.format;
                let mut colour = format.color;
                if colour == Color32::PLACEHOLDER {
                    colour = text_shape.override_text_color.unwrap_or(fallback);
                }
                if colour == Color32::PLACEHOLDER || colour.a() == 0 {
                    continue;
                }
                push_paint(capture, "text", colour, rect);
                capture.inks.push(Ink {
                    index,
                    text: words.clone(),
                    rect,
                    colour,
                    size: format.font_id.size,
                    face: Face::of(&format.font_id.family),
                });
            }
        }
        Shape::Mesh(mesh) => {
            let mut seen: Vec<Color32> = Vec::new();
            for vertex in &mesh.vertices {
                if !seen.contains(&vertex.color) {
                    seen.push(vertex.color);
                    push_paint(capture, "mesh vertex", vertex.color, rect);
                }
            }
            // A mesh is also how a filled polygon is painted, and a polygon of one colour is a
            // flat fill. What is not flat is a mesh whose vertices disagree (a ramp between
            // stops) or one that is a texture.
            let textured = mesh.texture_id != egui::TextureId::default();
            if textured {
                capture.foreign.push(Foreign {
                    kind: "a textured mesh".to_owned(),
                    rect,
                });
            } else if seen.len() > 1 {
                let stops: Vec<String> = seen
                    .iter()
                    .map(|colour| format!("{},{},{}", colour.r(), colour.g(), colour.b()))
                    .collect();
                capture.foreign.push(Foreign {
                    kind: format!(
                        "a mesh running between {} stops ({}) from ({:.0}, {:.0})",
                        seen.len(),
                        stops.join(" → "),
                        rect.left(),
                        rect.top()
                    ),
                    rect,
                });
            }
        }
        Shape::QuadraticBezier(curve) => {
            if curve.stroke.width > 0.0 {
                push_path_stroke(capture, "curve stroke", &curve.stroke.color, rect);
            }
        }
        Shape::CubicBezier(curve) => {
            if curve.stroke.width > 0.0 {
                push_path_stroke(capture, "curve stroke", &curve.stroke.color, rect);
            }
        }
        Shape::Callback(_) => {}
    }
}

// ---------------------------------------------------------------------------------- the set

/// Every screen at every size, painted once for the whole file: `cargo test` runs the tests of
/// one binary on several threads, and a frame is a hundredth of a second's worth of typing to
/// repeat per rule.
fn captures() -> &'static [Capture] {
    static ONCE: OnceLock<Vec<Capture>> = OnceLock::new();
    ONCE.get_or_init(|| {
        let archive = TempArchive::build();
        let mut all = Vec::new();
        for screen in SCREENS {
            for (width, height) in SIZES {
                all.push(archive.capture(screen, vec2(width, height)));
            }
        }
        all
    })
}

// ------------------------------------------------------------------------------- rule one

/// The palette as a name: what a colour is called, for a message.
fn palette_name(colour: Color32) -> Option<&'static str> {
    theme::ALL
        .iter()
        .find(|(_, value)| *value == colour)
        .map(|(name, _)| *name)
}

/// Rule one: every painted colour is an opaque grey the theme names.
///
/// There is no documented alpha step to allow: every entry of [`theme::ALL`] is
/// `Color32::from_gray` and therefore opaque, and nothing in `src/` fades a colour by hand
/// (`from_rgba`, `gamma_multiply`, `set_alpha` and `Color32::a()` have no caller there). A
/// translucent paint is therefore either egui's own (a disabled widget, a scroll fade) or a
/// colour that escaped the theme, and both are worth a line.
///
/// Grouped: a graph plate that paints ninety-nine edges in one off-palette grey is one finding
/// with a count, not ninety-nine lines nobody reads.
fn palette_findings() -> Tally {
    let mut findings = Tally::default();
    for capture in captures() {
        for paint in &capture.paints {
            let colour = paint.colour;
            let grey = colour.r() == colour.g() && colour.g() == colour.b();
            let named = palette_name(colour);
            let opaque = colour.a() == 255;
            if grey && named.is_some() && opaque {
                continue;
            }
            let verdict = if !grey {
                "has hue"
            } else if named.is_none() {
                "is a grey the palette does not name"
            } else {
                "is not opaque"
            };
            findings.add(format!(
                "{}: {} {} — rgb({},{},{}) alpha {}, on a {}×{} rect",
                capture.at(),
                paint.through,
                verdict,
                colour.r(),
                colour.g(),
                colour.b(),
                colour.a(),
                paint.rect.width().round() as i64,
                paint.rect.height().round() as i64
            ));
        }
    }
    findings
}

#[test]
fn every_painted_colour_is_an_opaque_grey_from_the_palette() {
    let findings = palette_findings();
    assert!(
        findings.is_empty(),
        "{} paints a colour the theme does not name, or a colour with alpha:\n{}",
        findings.len(),
        findings.listing()
    );
}

// ------------------------------------------------------------------------------- rule two

/// Rule two: nothing painted is a gradient, a texture, a glow or a shadow.
fn foreign_findings() -> Tally {
    let mut findings = Tally::default();
    for capture in captures() {
        for foreign in &capture.foreign {
            let size = if foreign.rect.height() > 0.0 {
                format!(
                    " {}×{} px",
                    foreign.rect.width().round() as i64,
                    foreign.rect.height().round() as i64
                )
            } else {
                String::new()
            };
            findings.add(format!("{}: {}{size}", capture.at(), foreign.kind));
        }
    }
    findings
}

#[test]
fn nothing_painted_is_a_gradient_texture_or_glow() {
    let findings = foreign_findings();
    assert!(
        findings.is_empty(),
        "{} shapes are not flat paint:\n{}",
        findings.len(),
        findings.listing()
    );
}

// ----------------------------------------------------------------------------- rule three

/// The surface a line of text lands on: the last fill painted before it that covers its
/// centre, or the page if there is none.
fn surface(capture: &Capture, ink: &Ink) -> Option<Fill> {
    let centre = ink.rect.center();
    capture
        .fills
        .iter()
        .rev()
        .find(|fill| fill.index < ink.index && fill.rect.contains(centre))
        .copied()
}

impl Capture {
    /// The surface colour under every line of text, in paint order.
    fn contrast_pairs(&self) -> Vec<(f64, String)> {
        let page = Fill {
            index: 0,
            rect: Rect::EVERYTHING,
            colour: theme::PAGE,
        };
        self.inks
            .iter()
            .map(|ink| {
                let under = surface(self, ink).unwrap_or(page);
                let ratio = theme::contrast(ink.colour, under.colour);
                (
                    ratio,
                    format!(
                        "{:.2}:1 {:?} {} px {} on rgb({},{},{})",
                        ratio,
                        sample(&ink.text),
                        ink.size,
                        ink.face.word(),
                        under.colour.r(),
                        under.colour.g(),
                        under.colour.b()
                    ),
                )
            })
            .collect()
    }
}

/// A failure message's copy of a line, short enough to read in a list.
fn sample(text: &str) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= 44 {
        return flat;
    }
    let cut: String = flat.chars().take(41).collect();
    format!("{cut}…")
}

#[test]
fn every_line_of_text_clears_45_to_1_against_its_surface() {
    let mut worst: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for capture in captures() {
        let pairs = capture.contrast_pairs();
        if let Some((ratio, pair)) = pairs
            .iter()
            .cloned()
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        {
            worst.push(format!("{}: {pair}", capture.at()));
            if ratio < CONTRAST_FLOOR {
                let under = pairs
                    .iter()
                    .filter(|(value, _)| *value < CONTRAST_FLOOR)
                    .count();
                failures.push(format!(
                    "{}: {under} of {} lines are under {CONTRAST_FLOOR}:1, worst {pair}",
                    capture.at(),
                    pairs.len()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "text is painted under the contrast floor ({CONTRAST_FLOOR}:1) on {} screens.\n\
         Worst pair per screen:\n{}\nUnder the floor:\n{}",
        failures.len(),
        worst.join("\n"),
        failures.join("\n")
    );
}

// ------------------------------------------------------------------------------ rule four

/// Rule four: the corners a rect is painted with are square or on the one radius.
fn radius_findings() -> Tally {
    let mut findings = Tally::default();
    for capture in captures() {
        for corner in &capture.corners {
            if RADII.contains(&corner.radius) {
                continue;
            }
            findings.add(format!(
                "{}: a {}×{} rect has a {} px corner",
                capture.at(),
                corner.rect.width().round() as i64,
                corner.rect.height().round() as i64,
                corner.radius
            ));
        }
    }
    findings
}

#[test]
fn every_painted_rect_has_a_corner_the_theme_allows() {
    let findings = radius_findings();
    assert!(
        findings.is_empty(),
        "{} rects are rounded off the theme's radius (allowed: {RADII:?}):\n{}",
        findings.len(),
        findings.listing()
    );
    // And the rule is not passing for want of rectangles: the documented radius is painted, so
    // a rect that came back with three corners on it would have been seen.
    let rounded: usize = captures()
        .iter()
        .map(|capture| {
            capture
                .corners
                .iter()
                .filter(|corner| corner.radius == theme::RADIUS)
                .count()
        })
        .sum();
    assert!(
        rounded > 0,
        "no painted rectangle carries theme::RADIUS ({}) — the corner rule measured nothing",
        theme::RADIUS
    );
}

/// The exception list itself, pinned: [`theme::RADIUS`] is 2, and it is the only radius the
/// theme documents (`theme.rs:127-131`). Raising it in the theme is a change to this file, not
/// a silent redesign of every corner in the window.
#[test]
fn the_one_radius_is_the_documented_two() {
    assert_eq!(
        theme::RADIUS, 2,
        "theme::RADIUS is no longer 2: the audit's exception list (square, or this) has to be \
         re-read against theme.rs before the interface grows a softer corner"
    );
}

// ------------------------------------------------------------------------------ rule five

/// Whether a length is a whole number of spacing units, within the 1 px a hairline between two
/// rectangles is painted over.
fn on_ladder(value: f32) -> bool {
    let nearest = (value / theme::space::UNIT).round() * theme::space::UNIT;
    (value - nearest).abs() <= 1.0
}

/// Rule five: the padding from a surface's own left edge to the column of text inside it is on
/// the scale.
///
/// A rectangle does not carry a gap: the distance *between* two blocks belongs to neither of
/// them, and the distance between two lines of text is line height as much as it is spacing. The
/// padding is the one number a painted surface does decide, and a column of text is where it can
/// be read: the left edge that a stack of lines agrees on, measured from the surface's own left
/// edge. Agreement is the point — a line centred in a button, or a sentence centred in an empty
/// panel, has no padding to measure, and measuring where centring happens to put it would be a
/// finding about arithmetic rather than about the scale.
fn spacing_findings() -> Tally {
    let mut findings = Tally::default();
    for capture in captures() {
        // The lines that land on each surface, by the surface's paint index.
        let mut on: BTreeMap<usize, Vec<&Ink>> = BTreeMap::new();
        for ink in &capture.inks {
            if let Some(fill) = surface(capture, ink) {
                on.entry(fill.index).or_default().push(ink);
            }
        }
        for (index, inks) in on {
            let Some(fill) = capture.fills.iter().find(|fill| fill.index == index) else {
                continue;
            };
            let Some(column) = column_edge(&inks) else {
                continue;
            };
            let padding = column - fill.rect.left();
            if padding < 0.0 || on_ladder(padding) {
                continue;
            }
            let first = inks
                .iter()
                .filter(|ink| (ink.rect.left() - column).abs() <= 1.0)
                .min_by(|a, b| {
                    a.rect
                        .top()
                        .partial_cmp(&b.rect.top())
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            findings.add(format!(
                "{}: {:.1} px of padding on the left of a {:?} surface ({}×{} px), first line {:?}",
                capture.at(),
                padding,
                palette_name(fill.colour).unwrap_or("unnamed"),
                fill.rect.width().round() as i64,
                fill.rect.height().round() as i64,
                first.map(|ink| sample(&ink.text)).unwrap_or_default()
            ));
        }
    }
    findings
}

/// The left edge a stack of lines agrees on, when at least two of them agree: the column's
/// padding is measurable only where the text is in a column.
fn column_edge(inks: &[&Ink]) -> Option<f32> {
    let mut edges: Vec<(i64, f32, usize)> = Vec::new();
    for ink in inks {
        let key = (f64::from(ink.rect.left()) * 2.0).round() as i64;
        match edges.iter_mut().find(|(seen, _, _)| *seen == key) {
            Some((_, _, count)) => *count += 1,
            None => edges.push((key, ink.rect.left(), 1)),
        }
    }
    edges
        .into_iter()
        .filter(|(_, _, count)| *count >= 2)
        .max_by_key(|(_, _, count)| *count)
        .map(|(_, edge, _)| edge)
}

#[test]
fn the_padding_inside_a_painted_surface_is_on_the_spacing_scale() {
    let findings = spacing_findings();
    assert!(
        findings.is_empty(),
        "{} insets are not a whole number of {} px units:\n{}",
        findings.len(),
        theme::space::UNIT,
        findings.listing()
    );
}

// ------------------------------------------------------------------------------- rule six

/// The (size, face) pairs the role table produces: the only two-number combinations the views
/// are allowed to set text in.
fn role_pairs() -> Vec<(u32, Face)> {
    theme::role::ALL
        .iter()
        .map(|role| {
            let face = Face::of(&role.weight.family());
            (role.size.to_bits(), face)
        })
        .collect()
}

/// Rule six: every line of text is on the ladder and in a role.
fn type_findings() -> Tally {
    let roles = role_pairs();
    let mut findings = Tally::default();
    for capture in captures() {
        for ink in &capture.inks {
            let on_ladder_size = theme::LADDER.contains(&ink.size);
            let in_role = roles.contains(&(ink.size.to_bits(), ink.face));
            if on_ladder_size && in_role {
                continue;
            }
            let why = match (on_ladder_size, in_role) {
                (false, _) => "a size off the ladder",
                (true, false) => "a (size, face) pair no role produces",
                (true, true) => "on the ladder",
            };
            findings.add(format!(
                "{}: {} px {} is {why} — {:?}",
                capture.at(),
                ink.size,
                ink.face.word(),
                sample(&ink.text)
            ));
        }
    }
    findings
}

#[test]
fn every_line_of_text_is_on_the_ladder_and_in_a_role() {
    let findings = type_findings();
    assert!(
        findings.is_empty(),
        "{} lines are set off the type scale ({:?}):\n{}",
        findings.len(),
        theme::LADDER,
        findings.listing()
    );
}

// ----------------------------------------------------------------------------- the report

/// The measurement the six rules come from, one line per screen and size. Printed by
/// [`the_audit_table`]; a failure of any rule above also prints the part of it that failed.
fn table() -> String {
    let palette = palette_findings();
    let foreign = foreign_findings();
    let radius = radius_findings();
    let spacing = spacing_findings();
    let kinds = type_findings();
    let mut lines = vec![format!(
        "{:<14}{:>10}{:>8}{:>8}{:>7}{:>8}{:>9}{:>9}{:>9}{:>9}  worst contrast pair",
        "screen", "size", "shapes", "colours", "lines", "flat", "palette", "radius", "spacing", "type"
    )];
    for capture in captures() {
        let at = capture.at();
        let count = |findings: &Tally| findings.count(&at);
        let pairs = capture.contrast_pairs();
        let worst = pairs
            .iter()
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, pair)| pair.as_str())
            .unwrap_or("no text");
        lines.push(format!(
            "{:<14}{:>10}{:>8}{:>8}{:>7}{:>8}{:>9}{:>9}{:>9}{:>9}  {}",
            capture.screen,
            format!("{}×{}", capture.size.x.round() as i64, capture.size.y.round() as i64),
            capture.shapes,
            capture.colour_count(),
            capture.inks.len(),
            count(&foreign),
            count(&palette),
            count(&radius),
            count(&spacing),
            count(&kinds),
            worst
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "palette ({} named greys): {}",
        theme::ALL.len(),
        theme::ALL
            .iter()
            .map(|(_, colour)| format!("{}", colour.r()))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    lines.push(format!(
        "ladder: {:?} · faces: {} / {} · radius: {} · unit: {}",
        theme::LADDER,
        theme::TEXT_FACE,
        theme::NUMBER_FACE,
        theme::RADIUS,
        theme::space::UNIT
    ));
    lines.join("\n")
}

#[test]
fn the_audit_table() {
    println!("{}", table());
    // The table is the deliverable; this test only fails if there was nothing to measure.
    for capture in captures() {
        assert!(
            !capture.inks.is_empty() && !capture.paints.is_empty(),
            "{} painted nothing: the audit measured an empty frame",
            capture.at()
        );
    }
}

// ------------------------------------------------------------------------------ messages

/// A finding list, grouped: the same finding on the same screen is one line with a count.
///
/// Four hundred lines of the same grey in a failure message is a message nobody reads, and the
/// count is the number that matters — one off-palette colour painted ninety-nine times is one
/// bug and ninety-nine paints.
#[derive(Default)]
struct Tally {
    lines: BTreeMap<String, usize>,
}

impl Tally {
    fn add(&mut self, line: String) {
        *self.lines.entry(line).or_insert(0) += 1;
    }

    /// How many times the finding was made, not how many lines describe it.
    fn len(&self) -> usize {
        self.lines.values().sum()
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// How many findings start with `prefix` — a screen and a size, in every message here.
    fn count(&self, prefix: &str) -> usize {
        self.lines
            .iter()
            .filter(|(line, _)| line.starts_with(prefix))
            .map(|(_, count)| count)
            .sum()
    }

    /// The first lines, each with the count of what it stands for. A message that pastes
    /// everything is a message nobody reads, so the tail is summed up.
    fn listing(&self) -> String {
        const SHOWN: usize = 24;
        let mut lines: Vec<String> = self
            .lines
            .iter()
            .take(SHOWN)
            .map(|(line, count)| {
                if *count > 1 {
                    format!("{line}  (×{count})")
                } else {
                    line.clone()
                }
            })
            .collect();
        if self.lines.len() > SHOWN {
            lines.push(format!("… and {} more", self.lines.len() - SHOWN));
        }
        lines.join("\n")
    }
}


