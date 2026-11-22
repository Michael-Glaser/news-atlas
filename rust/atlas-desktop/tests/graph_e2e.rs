//! The Graph page, end to end: the click chain the user says is broken.
//!
//! | claim | driven by |
//! |---|---|
//! | a node's circle opens its relations, strongest first, with their reasons | `Atlas::draw` |
//! | a relation row opens that pair and lists its stories | `Atlas::draw` |
//! | a story row switches the window to Articles with that story selected | `Atlas::draw` |
//! | a node with no relations says so, and Esc clears back to the prompt | `Atlas::draw` |
//! | a click on empty plate clears the selection | `Atlas::draw` |
//! | circle → relation → story → Articles, one window, one test | `Atlas::draw` |
//!
//! Everything asserted is **content**: entity names, mention counts, reason sentences,
//! story markers, and the page `Atlas::page` reports. Positions are used only to aim a click
//! at the pixel the window painted (a circle's centre, a row's own rect) and to order rows
//! the panel itself ordered, so a layout change does not rewrite a test and a repaint cannot
//! fake a pass.
//!
//! # Two defects this file found, and how it reports them
//!
//! The window used to build its graph with `GraphView::default()` (no archive, so the panel
//! read `crate::assets::archive()` — the machine's live archive) and to drop what
//! `graph::panel` and `GraphView::show` returned (so a story clicked in the panel changed no
//! page). Both were fixed in `src/app.rs` while this file was written: the graph now gets
//! `with_archive(path)` and `graph_page` acts on the answer. The regression guard is
//! `the_window_must_hand_the_graph_its_own_archive_and_open_what_it_returns`, a scan of
//! `src/app.rs` (`window_defects`) that names the live line numbers and the exact patch;
//! `the_scan_tells_the_old_code_from_the_fixed_code` proves that scan discriminates, on two
//! snippets of the code as it stood and as it is. `the_window_opens_the_story_a_click_in_its
//! _graph_panel_names` is the behavioural proof the same chain now works in the window.
//!
//! One claim is *not* the window's behaviour and is recorded rather than worked around:
//! clicking a relation row opens the relation (both ends ringed, its stories listed) instead
//! of moving the selection to the other end. That is what the view documents
//! (graph.rs's module doc), so it is what test (2) asserts; moving the selection to the other
//! end would be a change in `graph.rs`, which this file does not own.
//!
//! # Two traps in driving a graph by clicking it
//!
//! * **The layout moves.** A click reheats the spring, and a probe's coordinates are ~18 px
//!   stale a frame later — outside the canvas's own hit radius. Every node click here
//!   therefore settles first and aims at a circle the *same* frame painted
//!   (`click_entity_page`, `click_entity_window`).
//! * **`Esc` cannot be gated on focus.** egui 0.37 nulls the focused widget when it sees
//!   Escape, before the frame's widgets run (`egui-0.37.0/src/memory/mod.rs:598-599`), so
//!   `GraphView::show`'s `if response.has_focus()` guard never sees an Escape frame. Test (4)
//!   proves it is about Escape and not about focus, by stepping the selection with
//!   `ArrowRight` — which sits behind the same guard — first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Color32, Pos2, Rect, Shape, Vec2};

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::data::Archive;
use atlas_desktop::theme;
use atlas_desktop::views::graph::{self, GraphView};

const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);
const HOURS: i64 = 72;

/// The grey a node's circle is filled with (`graph.rs:732`), which is how a filled circle
/// on this page is told apart from the rail's mark and from a ring.
/// The sentence the relation column paints when nothing is selected, so "cleared" is
/// asserted as the reader sees it rather than by reading a field.
const PROMPT: &str = "Select an entity on the plate to see what connects it, then a \
relation to see the stories it is made of.";

/// The node ink: a dot is filled with the theme's own INK, not a grey of its own.
const NODE_GREY: Color32 = atlas_desktop::theme::INK;

/// The grey the canvas clears its own rectangle to (`graph.rs:67` `PLATE = 7`, painted at
/// `graph.rs:616`), which is
/// how the plate is told apart from the page, the rail and the status row — all of which
/// are filled rectangles too, and some of them bigger.
const PLATE_GREY: Color32 = Color32::from_rgb(7, 7, 7);

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

/// A `data/atlas.db` of this test's own, removed when the test ends.
///
/// Five entities with five different mention counts, so each drawn circle has its own
/// radius (`graph.rs:847` `node_radius`) and "the biggest circle" names one entity without
/// the test knowing the layout. Four of them are in one web; `Lonely Institute` is in none.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

static NEXT_ARCHIVE: AtomicU64 = AtomicU64::new(0);

impl TempArchive {
    fn build() -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-graph-e2e-{}-{}-{}",
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
             CREATE TABLE article_entities(article_id INTEGER, entity_id INTEGER);
             CREATE TABLE topics(
                 id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER,
                 hits INTEGER, last_hit TEXT);
             CREATE TABLE monitors(
                 id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER,
                 cadence_minutes INTEGER, last_run TEXT, hits INTEGER);
             CREATE TABLE alerts(id INTEGER PRIMARY KEY);",
        )
        .expect("the schema data.rs and graph.rs read");

        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'Marker Desk')",
            [],
        )
        .expect("a source");

        // Five entities, five mention counts, five radii: 400 -> 19.4 px, 100 -> 11.4,
        // 49 -> 9.0, 9 -> 5.8, 1 -> 4.2. The counts are the fixture's, and the radius is
        // the view's own function of them, so "the biggest circle is Alpha Union" is a
        // claim about the data rather than about the layout.
        // Five entities in the window, and one stale partner *outside* it. The stale one
        // is what makes `Lonely Institute` a node with no relations in the drawing:
        // `data::entities` (data.rs:534-538) returns only entities that appear in `edges`,
        // so a node without relations cannot be an entity without edges — it is an entity
        // whose only edge points at an entity the window does not contain. `GraphView::fill`
        // drops that row ("both ends must be in the set"), leaving the node alone, which is
        // exactly the state the panel's "Nothing connects this entity inside the window"
        // sentence exists for.
        let entities = [
            (1, "Alpha Union", "ORG", 400, 2),
            (2, "Beta Lines", "ORG", 100, 2),
            (3, "Gamma Port", "LOC", 49, 2),
            (4, "Delta Works", "ORG", 9, 2),
            (5, "Lonely Institute", "ORG", 1, 2),
            (6, "Stale Partner", "ORG", 3, 400),
        ];
        for (id, name, kind, count, hours_ago) in entities {
            conn.execute(
                "INSERT INTO entities(id, name, type, count, last_seen) VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, name, kind, count, stamp(hours_ago)],
            )
            .expect("an entity");
        }

        // Alpha has three relations, with three different weights and two kinds of reason:
        // one the model labelled, one it did not (the panel's fallback sentence), one
        // labelled again. Weights 9, 4, 1 — so "strongest first" is exactly the order
        // Beta, Gamma, Delta, and a panel listing them alphabetically would fail.
        let edges = [
            (1, 2, 9.0, "MARKER-ALPHA-BETA-REASON"),
            (1, 3, 4.0, ""),
            (1, 4, 1.0, "MARKER-ALPHA-DELTA-REASON"),
            // Lonely's only relation points outside the window, so the drawing keeps the
            // entity and drops the line.
            (5, 6, 2.0, "MARKER-NEVER-DRAWN"),
        ];
        for (a, b, weight, label) in edges {
            conn.execute(
                "INSERT INTO edges(a, b, weight, label) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![a, b, weight, label],
            )
            .expect("a relation");
        }

        // Three stories, two of them naming both ends of Alpha–Beta, one naming Alpha and
        // Gamma. `stories_for` reads `article_entities` twice and joins on the article.
        let stories = [
            (1, "MARKER-STORY-ALPHA-BETA-NEW", 1, 2),
            (2, "MARKER-STORY-ALPHA-BETA-OLD", 1, 2),
            (3, "MARKER-STORY-ALPHA-GAMMA", 1, 3),
        ];
        for (id, title, one, two) in stories {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', 1, ?3, ?4, ?5, 'DEU', 'Hamburg', ?6, 1, 1, 0, 54.0, 10.0)",
                rusqlite::params![
                    id,
                    title,
                    stamp(if id == 1 { 2 } else { 20 * id }),
                    format!("{title} — a summary."),
                    format!("{title}: the body."),
                    format!("https://example.invalid/{id}")
                ],
            )
            .expect("a story");
            for entity in [one, two] {
                conn.execute(
                    "INSERT INTO article_entities(article_id, entity_id) VALUES(?1, ?2)",
                    rusqlite::params![id, entity],
                )
                .expect("a mention");
            }
        }
        drop(conn);
        TempArchive { dir, file }
    }

    fn path(&self) -> &Path {
        &self.file
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

// ---------------------------------------------------------------------------
// one frame, and what it painted
// ---------------------------------------------------------------------------

/// What one frame of the Graph page painted.
#[derive(Default)]
struct Frame {
    /// Every string, one per line, in paint order.
    text: String,
    /// Every string with where it landed, for aiming a click and for reading a column's
    /// order off the page rather than off the code.
    labels: Vec<(String, Rect)>,
    /// Filled circles: the nodes (grey 210) and, if any, the rail's mark.
    dots: Vec<(Pos2, f32, Color32)>,
    /// Stroked circles: the rings the canvas draws around the selected node and the two
    /// ends of the open relation.
    rings: Vec<(Pos2, f32)>,
    /// Every filled rectangle, with its fill: the plate is one colour among them.
    rects: Vec<(Rect, Color32)>,
}

impl Frame {
    fn paints(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    /// The plate: the rectangle the canvas clears to its own grey (`graph.rs:613-617`).
    /// Colour, not size — the page and the rail are rectangles too, and bigger.
    fn plate(&self) -> Rect {
        self.rects
            .iter()
            .filter(|(_, fill)| *fill == PLATE_GREY)
            .map(|(rect, _)| *rect)
            .max_by(|left, right| {
                (left.width() * left.height())
                    .partial_cmp(&(right.width() * right.height()))
                    .expect("two finite areas")
            })
            .unwrap_or_else(|| {
                panic!(
                    "the page painted no {:?} plate; its rectangles were {:?}",
                    PLATE_GREY, self.rects
                )
            })
    }

    fn rect_of(&self, needle: &str) -> Option<Rect> {
        self.labels
            .iter()
            .find(|(text, _)| text == needle)
            .map(|(_, rect)| *rect)
    }

    /// The `y` a string was painted at, or a panic naming what the page did paint.
    fn y_of(&self, needle: &str) -> f32 {
        self.rect_of(needle)
            .unwrap_or_else(|| panic!("the frame never painted {needle:?}:\n{}", self.text))
            .center()
            .y
    }

    /// The page's node circles: the grey-filled ones, inside the plate.
    fn nodes(&self) -> Vec<(Pos2, f32)> {
        self.dots
            .iter()
            .filter(|(_, _, fill)| *fill == NODE_GREY)
            .map(|(at, radius, _)| (*at, *radius))
            .collect()
    }

    /// The node circles with the biggest one first: the radius is `node_radius(count)`, so
    /// this orders entities by their mentions, which is what the fixture controls.
    fn nodes_by_size(&self) -> Vec<(Pos2, f32)> {
        let mut nodes = self.nodes();
        nodes.sort_by(|left, right| right.1.total_cmp(&left.1));
        nodes
    }

    /// The mention count the panel printed for the selected entity (`Institution · 400 mentions`),
    /// which identifies an entity in this fixture: the counts are unique, and the canvas
    /// never prints this line.
    fn mentions(&self) -> Option<i64> {
        let line = self
            .labels
            .iter()
            .find(|(text, _)| text.ends_with(" mentions"))?;
        let head = line.0.split(" · ").next()?;
        let _ = head;
        let digits: String = line
            .0
            .trim_end_matches(" mentions")
            .chars()
            .rev()
            .take_while(|character| character.is_ascii_digit() || *character == ' ')
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        digits.replace(' ', "").parse().ok()
    }

    /// A string painted *inside the relation column*: the canvas paints entity names too,
    /// so a bare `rect_of` could aim at a label on the plate.
    fn panel_rect_of(&self, needle: &str) -> Option<Rect> {
        let left = self.panel_left();
        self.labels
            .iter()
            .find(|(text, rect)| text == needle && rect.left() >= left - 2.0)
            .map(|(_, rect)| *rect)
    }

    /// Where the relation column starts: the `x` of its own section line, so the test reads
    /// the column's edge from the page instead of guessing a margin.
    fn panel_left(&self) -> f32 {
        self.labels
            .iter()
            .find(|(text, _)| text.contains("relations, strongest first"))
            .map(|(_, rect)| rect.left())
            .unwrap_or(SCREEN.x - graph::PANEL_WIDTH - 30.0)
    }
}

fn record(shapes: &[egui::epaint::ClippedShape]) -> Frame {
    let mut frame = Frame::default();
    for clipped in shapes {
        walk(&clipped.shape, &mut frame);
    }
    frame
}

fn walk(shape: &Shape, frame: &mut Frame) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, frame);
            }
        }
        Shape::Text(text) => {
            let string = text.galley.text().to_string();
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            frame.labels.push((string.clone(), rect));
            frame.text.push_str(&string);
            frame.text.push('\n');
        }
        Shape::Circle(circle) if circle.fill.a() > 0 => {
            frame.dots.push((circle.center, circle.radius, circle.fill));
        }
        Shape::Circle(circle) => frame.rings.push((circle.center, circle.radius)),
        Shape::Rect(rect) => frame.rects.push((rect.rect, rect.fill)),
        _ => {}
    }
}

/// The input of one frame.
fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        events,
        ..Default::default()
    }
}

/// A context styled as the window styles it.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// One frame of the whole window, Graph page and all.
fn window_frame(ctx: &egui::Context, atlas: &mut Atlas, events: Vec<egui::Event>) -> Frame {
    let mut output = ctx.run_ui(input(events), |ui| atlas.draw(ui));
    output.textures_delta.clear();
    record(&output.shapes)
}

/// One frame of the Graph *page* — the canvas and the relation column, in the order
/// `app.rs` draws them — for the claims the window cannot reach without reading the
/// machine's archive.
///
/// Returns the frame and **the canvas's answer**: `GraphView::show` returns the id the
/// panel answered with one frame earlier (`graph.rs:837`), which is the value a shell that
/// opened stories would act on.
fn page_frame(
    ctx: &egui::Context,
    view: &mut GraphView,
    events: Vec<egui::Event>,
) -> (Frame, Option<i64>) {
    let mut answer: Option<i64> = None;
    let mut output = ctx.run_ui(input(events), |ui| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::PAGE))
            .show(ui, |ui| {
                let height = 640.0;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui(egui::vec2(1000.0, height), |ui| {
                        answer = view.show(ui, height);
                    });
                    ui.allocate_ui(egui::vec2(graph::PANEL_WIDTH, height), |ui| {
                        // The page's own call, in the page's own order: the answer is the
                        // story a reader clicked.
                        let _ = graph::panel(ui, view);
                    });
                });
            });
    });
    output.textures_delta.clear();
    (record(&output.shapes), answer)
}

/// Press and release the primary button at `at`, then draw the frame after it.
fn click(ctx: &egui::Context, atlas: &mut Atlas, at: Pos2) -> Frame {
    for pressed in [true, false] {
        window_frame(
            ctx,
            atlas,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    window_frame(ctx, atlas, vec![egui::Event::PointerMoved(at)])
}

/// The same click, on the page harness.
fn page_click(ctx: &egui::Context, view: &mut GraphView, at: Pos2) -> Frame {
    for pressed in [true, false] {
        page_frame(
            ctx,
            view,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    page_frame(ctx, view, vec![egui::Event::PointerMoved(at)]).0
}

/// Run frames until the layout stops moving: the nodes are spring-driven and settle over
/// `alpha`, so a click aimed before that would be aimed at where a node used to be.
///
/// Bounded, so a graph that never settles fails with a sentence instead of hanging.
fn settle(ctx: &egui::Context, atlas: &mut Atlas) -> Frame {
    let mut last: Option<Vec<Pos2>> = None;
    let mut still = 0;
    for _ in 0..600 {
        let frame = window_frame(ctx, atlas, Vec::new());
        let now: Vec<Pos2> = frame.nodes_by_size().iter().map(|(at, _)| *at).collect();
        if let Some(previous) = &last {
            let moved = previous
                .iter()
                .zip(&now)
                .map(|(was, is)| was.distance(*is))
                .fold(0.0_f32, f32::max);
            // The integrator converges asymptotically: measured, the biggest per-frame
            // move decays 3.3 px -> 0.4 px and keeps shrinking. Waiting for it to stop
            // entirely would be waiting forever, so "settled" is "slower than half a
            // pixel per frame", which is far inside the canvas's own hit radius
            // (`HIT_MIN + HIT_SLACK`, graph.rs:557-564).
            if previous.len() == now.len() && moved < 0.5 {
                still += 1;
            } else {
                still = 0;
            }
            if still >= 3 {
                return frame;
            }
        }
        last = Some(now);
    }
    panic!("the graph never stopped moving");
}

/// The window on this file's archive, on the Graph page, settled.
fn graph_window() -> (TempArchive, egui::Context, Atlas, Frame) {
    let temp = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.file.clone());
    atlas.set_page(Page::Graph);
    let frame = settle(&ctx, &mut atlas);
    (temp, ctx, atlas, frame)
}

/// A patch of plate with no node on it: the first inset corner of the plate that is at
/// least `clear` pixels from every painted node. Read off the frame, so it stays water
/// whatever the layout does.
fn water(frame: &Frame, clear: f32) -> Pos2 {
    let plate = frame.plate();
    let nodes = frame.nodes();
    let inside = plate.shrink(40.0);
    let corners = [
        inside.left_top(),
        inside.right_top(),
        inside.left_bottom(),
        inside.right_bottom(),
        inside.center(),
    ];
    corners
        .iter()
        .copied()
        .find(|corner| {
            nodes
                .iter()
                .all(|(at, _)| at.distance(*corner) > clear)
        })
        .unwrap_or_else(|| {
            panic!(
                "every corner of the plate {plate:?} has a node within {clear} px: {:?}",
                nodes
            )
        })
}

// ---------------------------------------------------------------------------
// identifying what is where, without guessing the layout
// ---------------------------------------------------------------------------

/// Which entity is behind which painted circle: click each circle in turn and read the
/// mention count the panel prints (`Institution · 400 mentions` — unique per entity in this
/// fixture, and the canvas never prints that line).
///
/// The canvas culls a node whose point falls outside the plate, so a test may not assume
/// "five entities, five circles at known indices"; it asks the window what it drew.
fn entities_on_plate(
    ctx: &egui::Context,
    atlas: &mut Atlas,
    mut frame: Frame,
) -> Vec<(i64, Pos2)> {
    let count = frame.nodes_by_size().len();
    let mut found: Vec<(i64, Pos2)> = Vec::new();
    for index in 0..count {
        let at = frame.nodes_by_size()[index].0;
        let after = click(ctx, atlas, at);
        if let Some(mentions) = after.mentions() {
            found.push((mentions, at));
        }
        frame = after;
    }
    found
}

/// Run page frames until the layout stops moving, by the same rule as [`settle`].
fn settle_page(ctx: &egui::Context, view: &mut GraphView) -> Frame {
    let mut last: Option<Vec<Pos2>> = None;
    let mut still = 0;
    for _ in 0..600 {
        let frame = page_frame(ctx, view, Vec::new()).0;
        let now: Vec<Pos2> = frame.nodes_by_size().iter().map(|(at, _)| *at).collect();
        if let Some(previous) = &last {
            let moved = previous
                .iter()
                .zip(&now)
                .map(|(was, is)| was.distance(*is))
                .fold(0.0_f32, f32::max);
            if previous.len() == now.len() && moved < 0.5 {
                still += 1;
            } else {
                still = 0;
            }
            if still >= 3 {
                return frame;
            }
        }
        last = Some(now);
    }
    panic!("the graph never stopped moving");
}

/// Click the node that answers with `mentions`, aiming only at circles the frame it is
/// clicked in painted.
///
/// Measured for the report: a circle located by a probe pass and clicked one frame later
/// missed — Beta Lines was at (330.8, 475.0) when probed and (344.4, 462.6) when
/// clicked, 18.4 px off against a hit radius of `11.4 + 3 = 14.4` (graph.rs:557-564), because
/// a click reheats the layout and the spring then moves nodes a few px per frame. Settling
/// before each attempt and using that attempt's own frame puts the release one ~0.5 px step
/// from the coordinate, well inside the radius. The loop is bounded, so a node that cannot be
/// hit fails with a sentence.
fn click_entity_page(ctx: &egui::Context, view: &mut GraphView, want: i64) -> Frame {
    for _ in 0..24 {
        settle_page(ctx, view);
        let frame = page_frame(ctx, view, Vec::new()).0;
        for (at, _) in frame.nodes_by_size() {
            let after = page_click(ctx, view, at);
            if after.mentions() == Some(want) {
                return after;
            }
        }
    }
    panic!("no circle on the plate answers with {want} mentions");
}

/// The same, on the window harness.
fn click_entity_window(ctx: &egui::Context, atlas: &mut Atlas, want: i64) -> Frame {
    for _ in 0..24 {
        let frame = settle(ctx, atlas);
        for (at, _) in frame.nodes_by_size() {
            let after = click(ctx, atlas, at);
            if after.mentions() == Some(want) {
                return after;
            }
        }
    }
    panic!("no circle on the window's plate answers with {want} mentions");
}

// ---------------------------------------------------------------------------
// (1) a node's circle opens its relations
// ---------------------------------------------------------------------------

/// Click a node's circle. The panel must then list exactly that node's relations — three,
/// because the fixture wrote three — strongest first, with each row's reason: the model's
/// own words where it wrote any, the honest fallback where it did not.
#[test]
fn clicking_a_node_lists_exactly_its_relations_strongest_first() {
    let (_temp, ctx, mut atlas, settled) = graph_window();
    let found = entities_on_plate(&ctx, &mut atlas, settled);
    assert!(
        found.len() >= 4,
        "the plate answered for only {} entities: {found:?}",
        found.len()
    );
    let after = click_entity_window(&ctx, &mut atlas, 400);

    assert!(
        after.paints("Alpha Union"),
        "the panel did not name the entity that was clicked:\n{}",
        after.text
    );
    assert!(
        // The panel states one vocabulary. The archive stores the legacy extractor's codes
        // (`ORG`, `PER`, `LOC`) next to the model's own words, and `graph.rs::type_words`
        // maps them at draw time: `ORG` is what an organisation *is*, so the row says
        // `Institution`. The id and the number are the same row the fixture wrote.
        after.paints("Institution · 400 mentions"),
        "the panel did not say how often the entity is mentioned:\n{}",
        after.text
    );
    assert!(
        after.paints("3 relations, strongest first"),
        "the panel did not count the relations the archive wrote:\n{}",
        after.text
    );
    // The two kinds of reason: the model's words carried through, and the fallback.
    assert!(
        after.paints("MARKER-ALPHA-BETA-REASON · 9 stories"),
        "the labelled relation did not carry its reason and its weight:\n{}",
        after.text
    );
    assert!(
        after.paints("named together in 4 stories"),
        "the unlabelled relation did not fall back to the count:\n{}",
        after.text
    );
    assert!(
        after.paints("MARKER-ALPHA-DELTA-REASON · 1 story"),
        "the weakest relation did not carry its reason and its weight:\n{}",
        after.text
    );
    // The rows, top to bottom, are the strongest first — read off the page.
    let beta = after.y_of("MARKER-ALPHA-BETA-REASON · 9 stories");
    let gamma = after.y_of("named together in 4 stories");
    let delta = after.y_of("MARKER-ALPHA-DELTA-REASON · 1 story");
    assert!(
        beta < gamma && gamma < delta,
        "the rows are not strongest first: Beta {beta:.1}, Gamma {gamma:.1}, Delta {delta:.1}"
    );
    // …and the canvas rings the node it selected, so the panel and the picture agree. The
    // ring is compared to the circles this *same* frame painted, not to the coordinate the
    // click used: the layout keeps moving (measured 18.4 px between two frames after a
    // click), so a remembered coordinate is not where the node is drawn now.
    // Only rings that sit on a painted node count: the window's own chrome rings its logo
    // (measured at (27.0, 29.0), radius 8.96) and this test is about the picture.
    let ringed: Vec<Pos2> = {
        let mut centres: Vec<Pos2> = Vec::new();
        for (at, _) in &after.rings {
            let on_a_node = after
                .nodes()
                .iter()
                .any(|(node, _)| node.distance(*at) < 1.0);
            if on_a_node && !centres.iter().any(|known| known.distance(*at) < 1.0) {
                centres.push(*at);
            }
        }
        centres
    };
    assert_eq!(
        ringed.len(),
        1,
        "one node is selected, so exactly one circle is ringed (the selected circle carries \
         two concentric strokes, the chrome's own rings are not on a node): {:?}",
        after.rings
    );
    assert!(
        after
            .nodes()
            .iter()
            .any(|(node, _)| node.distance(ringed[0]) < 1.0),
        "the ring at {:?} is not on a painted node: nodes {:?}",
        ringed[0],
        after.nodes()
    );
    assert!(
        !after.paints("Beta Lines · 100 mentions"),
        "the panel listed a neighbour as the selection:\n{}",
        after.text
    );
}

// ---------------------------------------------------------------------------
// (2) a relation row opens that pair; another node replaces the column in one frame
// ---------------------------------------------------------------------------

/// Clicking a relation row rings both ends and lists the stories that name both — read
/// from the archive the view was pointed at, never from the machine's. Then a click on
/// another node replaces the column in the very frame of the click: no stale list.
#[test]
fn a_relation_row_lists_its_pair_and_another_node_replaces_it_at_once() {
    let temp = TempArchive::build();
    let ctx = context();
    let archive = Archive::open(temp.path()).expect("the fixture opens");
    let entities = archive.entities(HOURS, 120);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let edges = archive.edges(&ids, 900);
    assert_eq!(
        edges.len(),
        3,
        "the fixture wrote four relations, one of which points outside the window"
    );

    let mut view = GraphView::default().with_archive(temp.path());
    view.fill(entities, edges);
    let selected = click_entity_page(&ctx, &mut view, 400);
    assert!(
        selected.paints("3 relations, strongest first"),
        "the page's own node click did not fill the column:\n{}",
        selected.text
    );

    // The unlabelled row (Alpha–Gamma) is a door like any other.
    let row = selected
        .panel_rect_of("Gamma Port")
        .expect("the panel painted no Gamma Port row")
        .center();
    let opened = page_click(&ctx, &mut view, row);

    let focus = view.focused().expect("the row did not open the pair");
    assert_ne!(focus.0, focus.1, "an open relation is between two nodes");
    assert!(
        opened.paints("The newest 1 story that name both"),
        "the open relation did not list its story count:\n{}",
        opened.text
    );
    assert!(
        opened.paints("MARKER-STORY-ALPHA-GAMMA"),
        "the open relation did not list the story that names both ends:\n{}",
        opened.text
    );
    assert!(
        !opened.paints("MARKER-STORY-ALPHA-BETA-NEW"),
        "the open relation listed a story that names only one end:\n{}",
        opened.text
    );
    // Both ends are ringed, on the canvas, in the frame the panel filled. The nodes drift
    // ---- the three facts a relation row has to carry ----
    //
    // The user's complaint in one place: "welche Verbindung die haben oder warum die
    // miteinander verbunden sind". A row states the far end's name (its own title), the
    // relation's weight in stories, the reason the archive holds for it — and, once opened,
    // the strongest story behind it, read from the stories this view already holds, so the
    // row costs no query of its own.
    let alpha = view.entities[focus.0].id;
    let gamma = view.entities[focus.1].id;
    let stories = graph::stories_for(temp.path(), alpha.min(gamma), alpha.max(gamma), 5)
        .expect("the fixture's relation has stories");
    let strongest = stories.first().expect("a story names both ends");
    assert!(
        opened.paints("Gamma Port"),
        "the opened row does not name the far end:\n{}",
        opened.text
    );
    assert!(
        opened.paints("named together in 4 stories"),
        "the opened row does not state the relation's weight in stories:\n{}",
        opened.text
    );
    let named = format!("\"{}\" (story {})", strongest.title, strongest.id);
    assert!(
        opened.paints(&named),
        "the opened row does not name the strongest story behind it ({named:?}):\n{}",
        opened.text
    );
    // …and that story is the door the row promised: the press's own frame answers with its id
    // (the panel's answer is immediate; the canvas's copy of it is the next frame's).
    let story_row = opened
        .panel_rect_of(&strongest.title)
        .expect("the strongest story is not painted in the panel")
        .center();
    let button = |pressed: bool| egui::Event::PointerButton {
        pos: story_row,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    page_frame(&ctx, &mut view, vec![egui::Event::PointerMoved(story_row)]);
    page_frame(&ctx, &mut view, vec![button(true)]);
    let (_, immediate) = page_frame(&ctx, &mut view, vec![button(false)]);
    // `show` hands the panel's answer over on the next frame (its doc says so), so the press
    // frame and the frame after it are both read: the story must arrive in one of them.
    let (_, after) = page_frame(&ctx, &mut view, Vec::new());
    let answer = immediate.or(after);
    assert_eq!(
        answer,
        Some(strongest.id),
        "a press on the story the row names did not open story {}",
        strongest.id
    );
    // a fraction of a pixel per frame, so this compares the rings to the circles the *same
    // frame* painted instead of to the coordinates the probe saw earlier.
    let ringed = {
        let mut centres: Vec<Pos2> = Vec::new();
        for (at, _) in &opened.rings {
            if !centres.iter().any(|known| known.distance(*at) < 1.0) {
                centres.push(*at);
            }
        }
        centres
    };
    assert_eq!(
        ringed.len(),
        2,
        "an open relation rings exactly its two ends (plus the selected node); this frame \
         rang {:?}",
        opened.rings
    );
    for at in &ringed {
        assert!(
            opened
                .nodes()
                .iter()
                .any(|(node, _)| node.distance(*at) < 1.0),
            "a ring was drawn at {at:?}, which is not a node: nodes {:?}",
            opened.nodes()
        );
    }

    // Another node: the column is that node's relations in the frame of the click.
    let swapped = click_entity_page(&ctx, &mut view, 100);
    assert!(
        swapped.paints("Beta Lines"),
        "the panel did not move to the node that was clicked:\n{}",
        swapped.text
    );
    assert!(
        !swapped.paints("MARKER-STORY-ALPHA-GAMMA"),
        "the panel kept the previous relation's stories open over a new selection:\n{}",
        swapped.text
    );
    assert!(
        !swapped.paints("3 relations, strongest first"),
        "the panel kept the previous entity's relation count:\n{}",
        swapped.text
    );
    assert!(
        swapped.paints("1 relations, strongest first"),
        "the panel did not count the new entity's single relation:\n{}",
        swapped.text
    );
}

// ---------------------------------------------------------------------------
// (3) the answer a story row gives
// ---------------------------------------------------------------------------

/// A story row is a door: the id the panel answers with is the story the fixture put behind
/// that relation, and the canvas hands the same id to the shell one frame later
/// (`graph.rs:837` — `show` returns what the panel wrote).
#[test]
fn a_story_row_answers_with_the_story_behind_the_relation() {
    let temp = TempArchive::build();
    let ctx = context();
    let archive = Archive::open(temp.path()).expect("the fixture opens");
    let entities = archive.entities(HOURS, 120);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let mut view = GraphView::default().with_archive(temp.path());
    view.fill(entities, archive.edges(&ids, 900));

    let selected = click_entity_page(&ctx, &mut view, 400);
    let beta_row = selected
        .panel_rect_of("Beta Lines")
        .expect("the panel painted no Beta Lines row")
        .center();
    let opened = page_click(&ctx, &mut view, beta_row);
    assert!(
        opened.paints("MARKER-STORY-ALPHA-BETA-NEW")
            && opened.paints("MARKER-STORY-ALPHA-BETA-OLD"),
        "the relation did not open its two stories:\n{}",
        opened.text
    );
    assert!(
        opened.y_of("MARKER-STORY-ALPHA-BETA-NEW")
            < opened.y_of("MARKER-STORY-ALPHA-BETA-OLD"),
        "the stories are not newest first:\n{}",
        opened.text
    );

    // Press the older row: the *panel* answers in that frame, and the *canvas* answers in
    // the next one — the two readers of the same `Cell` (`GraphView::show`'s `opened`
    // take, and `graph::panel`'s `view.opened.set`).
    let story_row = opened
        .panel_rect_of("MARKER-STORY-ALPHA-BETA-OLD")
        .expect("the panel painted no story row")
        .center();
    for pressed in [true, false] {
        page_frame(
            &ctx,
            &mut view,
            vec![
                egui::Event::PointerMoved(story_row),
                egui::Event::PointerButton {
                    pos: story_row,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    let (frame, answer) = page_frame(&ctx, &mut view, vec![egui::Event::PointerMoved(story_row)]);
    assert!(
        frame.paints("MARKER-STORY-ALPHA-BETA-OLD"),
        "the story row was not on screen when it was clicked:\n{}",
        frame.text
    );
    assert_eq!(
        answer,
        Some(2),
        "the canvas did not hand the shell the story the panel opened: the row for \
         MARKER-STORY-ALPHA-BETA-OLD is article 2 in the fixture"
    );
}

// ---------------------------------------------------------------------------
// (4) nothing connects it, and Esc goes back to the prompt
// ---------------------------------------------------------------------------

/// A node with no relations says so in a sentence — not with an empty column — and `Esc`
/// returns the page to the prompt it opens with.
#[test]
fn a_node_with_no_relations_says_so_and_esc_returns_to_the_prompt() {
    let (_temp, ctx, mut atlas, settled) = graph_window();
    let found = entities_on_plate(&ctx, &mut atlas, settled);
    assert!(
        found.iter().any(|(mentions, _)| *mentions == 1),
        "the plate did not paint the weak node this test is about: {found:?}"
    );
    // `Lonely Institute` is in the window and in the edge table, but its only relation
    // points at an entity outside the window, so the drawing keeps the node and drops the
    // line: that is the state this sentence is for.
    let said = click_entity_window(&ctx, &mut atlas, 1);
    assert!(
        said.paints("Lonely Institute"),
        "the panel did not select the node with no relations:\n{}",
        said.text
    );
    assert!(
        said.paints(
            "Nothing connects this entity inside the window. Widen the window in the rail \
             to see more."
        ),
        "a node with no relations got no sentence:\n{}",
        said.text
    );
    assert!(
        !said.paints("relations, strongest first"),
        "the panel listed relations for an entity that has none:\n{}",
        said.text
    );

    // ---- control (1): the canvas does take the keyboard
    //
    // `ArrowRight` steps the selection and sits behind the same guard as `Esc`, so a step
    // here proves the keys reach the canvas at all.
    let step = |ctx: &egui::Context, atlas: &mut Atlas, key: egui::Key| {
        window_frame(
            ctx,
            atlas,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        )
    };
    let stepped = step(&ctx, &mut atlas, egui::Key::ArrowRight);
    assert!(
        stepped.mentions() != Some(1),
        "the canvas did not take the keyboard: ArrowRight, behind the same guard as Esc, \
         changed nothing:\n{}",
        stepped.text
    );

    // ---- control (2): `Esc` is the right key
    //
    // One key press after a fresh click, `Esc` does clear the selection — so this test is
    // not about Esc being wired to the wrong thing.
    let again = click_entity_window(&ctx, &mut atlas, 400);
    assert!(
        again.mentions() == Some(400),
        "the second click did not select the strong node:\n{}",
        again.text
    );
    let cleared_once = step(&ctx, &mut atlas, egui::Key::Escape);
    assert!(
        cleared_once.mentions().is_none(),
        "Esc did not clear the selection on the frame right after a click:\n{}",
        cleared_once.text
    );

    // ---- the defect: the canvas answers two keys and then goes deaf
    //
    // Measured on this build, in this order: click → `Some(1)`; ArrowRight → `Some(400)`;
    // ArrowRight → `Some(100)`; ArrowRight → `Some(100)`; Esc → `Some(100)`. The third key
    // in a row is ignored, and every key after it, until the next click — `GraphView::show`
    // ends every frame with `self.had_focus = response.has_focus()`, and `has_focus()` is
    // false on every frame after the click that requested it, so the flag decays one frame
    // later and the guard `self.had_focus || response.has_focus()` is false from then on.
    let reselected = click_entity_window(&ctx, &mut atlas, 1);
    assert!(
        reselected.mentions() == Some(1),
        "the weak node was not selected again:\n{}",
        reselected.text
    );
    // Two keys, then the third: measured, the canvas answers the first two and ignores the
    // rest, so `Esc` has to be pressed third to show the decay.
    let first = step(&ctx, &mut atlas, egui::Key::ArrowRight);
    let second = step(&ctx, &mut atlas, egui::Key::ArrowRight);
    let cleared = {
        let mut frame = step(&ctx, &mut atlas, egui::Key::Escape);
        for _ in 0..2 {
            frame = step(&ctx, &mut atlas, egui::Key::Escape);
        }
        frame
    };
    assert!(
        cleared.mentions().is_none() && cleared.paints(PROMPT),
        "Esc did not return the panel to its prompt after a key press, though it does on the \
         frame right after a click — so the key is right and the canvas has the keyboard, \
         and what decays is the focus flag. Measured: click → {rea}, ArrowRight → {one}, \
         then Escape ×3 → {now}. `src/views/graph.rs` keeps the keys behind \
         `let focused = response.has_focus() || self.had_focus;` and then runs \
         `self.had_focus = response.has_focus();` — but `has_focus()` stops being true a \
         frame or two after the click that requested it (measured: two presses answered, the \
         third ignored), so a flag copied from it decays to false and stays false until the \
         next click. Keeping it alive on the frame that used it fixes it: \
         `self.had_focus = response.has_focus() || focused;` — or re-request the focus, \
         `if focused && !response.has_focus() {{ response.request_focus(); }}`. The panel \
         after the third Escape:\n{text}",
        rea = "Some(1)",
        one = format!(
            "{} then {}",
            first.mentions().map_or("nothing".to_string(), |m| m.to_string()),
            second.mentions().map_or("nothing".to_string(), |m| m.to_string())
        ),
        now = cleared.mentions().map_or("nothing".to_string(), |m| m.to_string()),
        text = cleared.text
    );
}

// ---------------------------------------------------------------------------
// (5) a click on empty plate clears the selection
// ---------------------------------------------------------------------------

/// The canvas documents this behaviour in its own click handler (`graph.rs:659-667`: "The
/// reader is looking at another part of the picture: the relation the panel had open
/// belonged to the old selection, so the panel closes it") — a click that hits no node
/// clears the selection rather than leaving it behind.
#[test]
fn a_click_on_empty_plate_clears_the_selection() {
    let (_temp, ctx, mut atlas, settled) = graph_window();
    let found = entities_on_plate(&ctx, &mut atlas, settled);
    assert!(
        !found.is_empty(),
        "the plate painted no nodes to select from: {found:?}"
    );
    let selected = click_entity_window(&ctx, &mut atlas, 400);
    assert!(
        selected.paints("Alpha Union"),
        "the first click did not select a node:\n{}",
        selected.text
    );

    // A point on the plate at least 60 px from every node, read off the frame's own
    // geometry.
    let current = window_frame(&ctx, &mut atlas, Vec::new());
    let empty = water(&current, 60.0);
    let cleared = click(&ctx, &mut atlas, empty);
    assert!(
        cleared.paints(
            "Select an entity on the plate to see what connects it, then a relation to see \
             the stories it is made of."
        ),
        "a click on empty plate did not clear the selection:\n{}",
        cleared.text
    );
    assert!(
        cleared.mentions().is_none(),
        "the panel still names an entity after a click on empty plate:\n{}",
        cleared.text
    );
}

// ---------------------------------------------------------------------------
// the tripwire: the window throws the graph's answer away
// ---------------------------------------------------------------------------

/// **This test fails on the code as it stands, on purpose.**
///
/// The panel's answer is a story id, documented as the shell's (graph.rs's module doc: "The
/// answer is the story the reader clicked … a caller that reads it here switches pages
/// without waiting for the next frame"), and `GraphView::show` returns the same id a frame
/// later. `Atlas` does neither of the two things that needs:
///
/// * `src/app.rs:236` builds the graph with `GraphView::default()`, so the panel's archive
///   is `crate::assets::archive()` — the machine's live archive — instead of the archive
///   the window opened. A window on any other archive lists stories that are not there.
/// * `src/app.rs:586` and `:589` call `self.graph.show(...)` and `graph::panel(ui,
///   &self.graph)` for effect only: the returned `Option<i64>` is dropped, so a clicked
///   story leaves the reader on the Graph page.
///
/// The behavioural version of this test cannot be written honestly: driving it would read
/// `data/atlas.db`, which is someone's reading history and not this test's fixture. So the
/// tripwire reads the two lines instead, and says what to write. Delete it when the window
/// does the two things — a behavioural test of the page switch is then three lines of this
/// file (click node, click relation, click story; assert `Page::Articles` and the title).
// ---------------------------------------------------------------------------
// (6) the whole chain through the window
// ---------------------------------------------------------------------------

/// Circle → relation row → story row → Articles, all through `Atlas::draw`, on one window.
///
/// This is the test the user's complaint asks for. It could not exist until `src/app.rs`
/// handed its graph the archive it was opened on (`app.rs:266`,
/// `GraphView::default().with_archive(path)`) and routed the answer `graph::panel` returns
/// (`graph_page`: `articles_view.selected` + `set_page(Page::Articles)`); before that the
/// panel below read `data/atlas.db` and the story click changed no page. Both are in place
/// now, so nothing here is simulated: every assertion is about a pixel the window painted
/// and the page the window reports through `Atlas::page`.
#[test]
fn the_window_opens_the_story_a_click_in_its_graph_panel_names() {
    let temp = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.path().to_path_buf());
    atlas.set_page(Page::Graph);
    let settled = settle(&ctx, &mut atlas);

    // The window's graph is reading *this* archive: the fixture's strongest entity is on
    // its plate, and a window that read another archive would not name it.
    assert!(
        settled.paints("Alpha Union"),
        "the window's Graph page did not paint the fixture's strongest entity, so its graph \
         is not reading the archive the window was opened on; it painted:\n{}",
        settled.text
    );

    let selected = click_entity_window(&ctx, &mut atlas, 400);
    assert!(
        selected.paints("3 relations, strongest first"),
        "the window's panel did not count the relations the fixture wrote:\n{}",
        selected.text
    );
    assert!(
        selected.paints("MARKER-ALPHA-BETA-REASON · 9 stories"),
        "the window's panel did not carry the relation's reason:\n{}",
        selected.text
    );

    let row = selected
        .panel_rect_of("Beta Lines")
        .expect("the window's panel painted no Beta Lines row")
        .center();
    let opened = click(&ctx, &mut atlas, row);
    assert!(
        opened.paints("MARKER-STORY-ALPHA-BETA-OLD"),
        "the window's panel did not list the stories behind the relation it opened:\n{}",
        opened.text
    );

    let story = opened
        .panel_rect_of("MARKER-STORY-ALPHA-BETA-OLD")
        .expect("the window's panel painted no story row")
        .center();
    for pressed in [true, false] {
        window_frame(
            &ctx,
            &mut atlas,
            vec![
                egui::Event::PointerMoved(story),
                egui::Event::PointerButton {
                    pos: story,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    // The page is chosen at the top of a frame, so the frame after the click is the one
    // that paints Articles.
    let after = window_frame(&ctx, &mut atlas, vec![egui::Event::PointerMoved(story)]);
    assert_eq!(
        atlas.page(),
        Page::Articles,
        "a story clicked in the graph's panel left the reader on {:?}; the panel's answer is \
         documented as the shell's to act on. The page painted:\n{}",
        atlas.page(),
        after.text
    );
    assert!(
        after.paints("MARKER-STORY-ALPHA-BETA-OLD"),
        "the window switched to Articles but did not paint the story the click named:\n{}",
        after.text
    );
}

/// The line of the `has_focus()` guard that gates the canvas's keys, read out of
/// `src/views/graph.rs` — which this test must not edit. Scanning instead of writing the
/// number down keeps the failure message true while graph.rs is reworked next door.
fn esc_guard_line() -> String {
    let source = include_str!("../src/views/graph.rs");
    source
        .lines()
        .position(|line| line.trim() == "if response.has_focus() && !self.nodes.is_empty() {")
        .map(|index| format!("src/views/graph.rs:{}", index + 1))
        .unwrap_or_else(|| "the `has_focus()` guard in `GraphView::show`".to_string())
}

/// Where `had_focus` is declared, and how often the file mentions it: today once, i.e.
/// declared and never used — the stub of the fix above.
fn had_focus_line() -> String {
    let source = include_str!("../src/views/graph.rs");
    source
        .lines()
        .position(|line| line.trim() == "had_focus: bool,")
        .map(|index| format!("src/views/graph.rs:{}", index + 1))
        .unwrap_or_else(|| "the `had_focus` field".to_string())
}

fn had_focus_count() -> usize {
    include_str!("../src/views/graph.rs").matches("had_focus").count()
}

/// The module doc line that promises a clicked story opens, so the wiring message quotes the
/// view's own promise instead of a line number that moves.
fn panel_promise_line() -> String {
    let source = include_str!("../src/views/graph.rs");
    source
        .lines()
        .position(|line| line.contains("A relation row in the panel is not a caption"))
        .map(|index| format!("src/views/graph.rs:{}", index + 1))
        .unwrap_or_else(|| "the graph view's module doc".to_string())
}

/// What is wrong with the window's Graph page, read out of `src/app.rs` — which this test
/// must not edit, because another agent owns it. Returns one sentence per defect; empty means
/// the window is honest.
///
/// Why a scan and not a click: defect (1) means the window's graph reads
/// `crate::assets::archive()` (graph.rs:357-363, the machine's live archive) instead of the
/// archive `Atlas::with_archive` was given, so no fixture's content can be observed through
/// the window at all — a behavioural test would be reading `data/atlas.db`. Defect (2) is
/// only visible *after* (1) is fixed. graph.rs's own tests use the same `include_str!` scan
/// for the same reason.
fn window_defects(app: &str) -> Vec<String> {
    let mut defects = Vec::new();

    // (1) the graph view is never told which archive the window opened.
    let graph_line = app.lines().enumerate().find_map(|(index, line)| {
        (line.contains("graph: GraphView::default()") && !line.contains(".with_archive"))
            .then_some(index + 1)
    });
    if let Some(line) = graph_line {
        defects.push(format!(
            "src/app.rs:{line} builds `graph: GraphView::default()`, so the relation panel \
             reads `crate::assets::archive()` — the machine's live archive (graph.rs:357-363) — \
             instead of the archive the window opened, and no test's fixture can ever reach \
             the panel.\n\n\
             The fix, in `Atlas::with_archive(path)` (one line):\n\
             \x20   graph: GraphView::default().with_archive(path.clone()),"
        ));
    }

    // (2) what the plate and the panel answer is dropped.
    let page = app
        .split("fn graph_page(&mut self, ui: &mut egui::Ui) {")
        .nth(1)
        .expect("app.rs has no `graph_page`, so this scan would check nothing");
    let page = page.split("\n    }\n").next().unwrap_or(page);
    // Three accepted spellings, because there are three ways to route the answer and the
    // shell uses the narrowest one: the plate and the panel hand back a story id, and
    // `Atlas::open_article` is the door every other list in the window uses for exactly that
    // (it selects the row, asks for it to be scrolled into view, and switches the page). A
    // scan that only knew the two inline spellings would fail a shell that routes the answer
    // *better* than the sentence it is checking.
    if !(page.contains("Page::Articles")
        || page.contains("articles_view.selected")
        || page.contains("self.open_article("))
    {
        let panel_line = app
            .lines()
            .enumerate()
            .find_map(|(index, line)| line.contains("graph::panel(ui, &self.graph)").then_some(index + 1));
        defects.push(format!(
            "src/app.rs:{} drops what the graph returned (`graph::panel(...)` and \
             `self.graph.show(...)` are statements), so a story clicked in the relation panel \
             leaves the reader on the Graph page while {} documents the opposite.\n\n\
             The fix, in `graph_page`: route the answer — take the panel's story (or \
             `self.graph.take_opened_story()`), then `self.articles_view.selected = Some(id)` \
             and `self.set_page(Page::Articles)`.",
            panel_line
                .map(|line| line.to_string())
                .unwrap_or_else(|| "the `graph::panel` call".to_string()),
            panel_promise_line()
        ));
    }

    defects
}

#[test]
fn the_window_must_hand_the_graph_its_own_archive_and_open_what_it_returns() {
    let defects = window_defects(include_str!("../src/app.rs"));
    assert!(
        defects.is_empty(),
        "the window's Graph page is not wired to its archive and drops its answers — {} \
         defect(s):\n\n{}",
        defects.len(),
        defects.join("\n\n")
    );
}

#[test]
fn the_scan_tells_the_old_code_from_the_fixed_code() {
    // How the scan above was verified to discriminate: the same scanner on a snippet of the
    // code as it stood (graph built without an archive, both answers dropped) must name both
    // defects, and on the same snippet with the fix applied must name none. The snippets are
    // written here rather than read from app.rs, so this proof keeps working once the fix
    // lands — which it has: app.rs:266 now passes the archive and `graph_page` routes the
    // answer, so this pair of snippets is the only place the old shape still exists.
    let old = "            graph: GraphView::default(),\n        }\n    fn graph_page(&mut self, ui: &mut egui::Ui) {\n        graph::panel(ui, &self.graph);\n    }\n";
    let fixed = "            graph: GraphView::default().with_archive(path),\n        }\n    fn graph_page(&mut self, ui: &mut egui::Ui) {\n        if let Some(id) = graph::panel(ui, &self.graph) {\n            self.articles_view.selected = Some(id);\n            self.set_page(Page::Articles);\n        }\n    }\n";
    assert_eq!(
        window_defects(old).len(),
        2,
        "the scan did not see both defects in the old shape: {:?}",
        window_defects(old)
    );
    assert!(
        window_defects(fixed).is_empty(),
        "the scan still complains about the fixed shape: {:?}",
        window_defects(fixed)
    );
}



