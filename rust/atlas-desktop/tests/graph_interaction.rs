//! The Graph page as a reader meets it: the specification, not the implementation.
//!
//! This file owns the *behaviour* of the graph canvas and its panel. It draws real frames in a
//! headless `egui::Context`, sends the events a pointer and a keyboard send, and asks the
//! picture what happened — never the fields alone. What a click did is read back two ways: from
//! the state the view publishes (`selected`, `pan`, `zoom`, `nodes`) and from the shapes the
//! frame painted (node circles, line segments, text, the plate). When the two disagree, the
//! reader is looking at something other than what the page thinks it is showing, and that is
//! what these tests are for.
//!
//! Six claims, one section each:
//!
//! 1. **A click lands on the node under the pointer** — at the opening zoom, at ×4, and after the
//!    plate has been panned 300 px in both axes. The click is aimed at the centre of a *painted*
//!    circle, and the panel is then read to confirm it describes that node and lists its
//!    relations.
//! 2. **One gesture per target**: a drag that starts on a node moves that node and leaves the
//!    canvas alone; a drag that starts on empty plate moves the canvas and touches no node.
//! 3. **The wheel zooms about the pointer**: the node under the pointer keeps its screen position
//!    within a pixel, six steps in and six steps out.
//! 4. **There is a way back**: a double click on empty plate, and Home with the canvas focused,
//!    each bring every node back inside the canvas. Both are stated as requirements — if the
//!    page has no such affordance, the test fails and says what is missing.
//! 5. **The keyboard is a way in**: Tab focuses the canvas and shows it, the arrows walk the
//!    nodes in a stable order, Esc clears, and the node the arrows leave selected is the node the
//!    panel describes.
//! 6. **Nothing leaks out of the canvas and nothing is NaN**, over a 400-event storm at three
//!    canvas sizes: every shape is clipped to the canvas, no node is drawn about a centre
//!    outside it, and no coordinate in the frame is infinite or NaN.
//!
//! Beyond the six, four smaller claims are part of "the picture is a page a reader can use": a
//! click on the empty plate clears the selection, a click never moves the camera, the arrows do
//! nothing while the canvas is unfocused, and — drawn in the page's real shape, canvas and panel
//! inside one horizontal row — the panel is a *column* of rows beside the canvas and not a strip
//! of text running across it
//! ([`the_panel_is_a_column_of_rows_beside_the_canvas`]). [`the_picture_is_where_the_view_says_it_is`] states
//! the agreement the rest of the file relies on: every painted circle is a node at its published
//! position, every relation is drawn between the two circles it joins, and every node inside the
//! canvas is painted.
//!
//! ## What fails today
//!
//! Three tests are red, and each one is a claim about the page rather than a mistake here:
//!
//! * [`a_double_click_on_the_empty_plate_brings_the_whole_graph_back`] — there is no way back to
//!   the whole graph after a pan; `views::graph` has no `double_clicked()` anywhere.
//! * [`home_with_the_canvas_focused_brings_the_whole_graph_back`] — nor is there a key for it:
//!   `Key::Home` is not read, and there is no fit/reset in the module.
//! * [`the_keyboard_walks_the_nodes_and_the_panel_follows`] — Esc does not clear the selection.
//!   The handler is gated on `response.has_focus()` (`views/graph.rs:675`), and egui clears the
//!   focused widget itself when it sees Escape (`egui-0.37.0/src/memory/mod.rs:597`), so the gate
//!   is always shut on the one frame the key arrives: the canvas loses focus, keeps its
//!   selection, and stops answering the arrows.
//!
//! The two gestures the tests invent (a double click, and Tab) are checked against plain egui
//! widgets first, in [`the_gestures_this_file_sends_are_the_gestures_a_reader_makes`]: a
//! specification that failed because the *harness* was wrong would be worse than no test.
//!
//! Nothing here opens a window, starts a process, or reads the live archive: the graph is filled
//! from the same `Entity`/`Edge` rows `app::Atlas` fills it with, and drawn from memory.

use atlas_desktop::data::{Edge, Entity};
use atlas_desktop::theme;
use atlas_desktop::views::graph::{panel, GraphView};
use egui::{Color32, Key, Pos2, Rect, Vec2};

/// The plate the graph is painted on (`views::graph`'s near-black), read back out of the frame.
const PLATE: Color32 = Color32::from_rgb(7, 7, 7);
/// A node's fill, likewise (`circle_filled(…, 210)`): the one grey a node is.
/// The node ink: a dot is filled with the theme's own INK, not a grey of its own.
const NODE: Color32 = atlas_desktop::theme::INK;

// --------------------------------------------------------------------------- the fixture

/// Ten entities on a ring, each joined to the next two: every node has relations to list, and
/// the picture has no hub that could swallow its neighbours.
///
/// The layout is run to rest and then frozen (`alpha = 0`), so a difference between two frames is
/// a difference a gesture made and not one the simulation made in the meantime.
fn graph(count: usize) -> GraphView {
    let entities: Vec<Entity> = (0..count)
        .map(|index| Entity {
            id: index as i64 + 1,
            name: format!("Entity {}", index + 1),
            kind: "ORG".to_string(),
            count: 9 - (index as i64 % 7),
        })
        .collect();
    let mut edges: Vec<Edge> = Vec::new();
    for index in 0..count {
        for step in [1, 2] {
            edges.push(Edge {
                a: index as i64 + 1,
                b: ((index + step) % count) as i64 + 1,
                weight: 1.0 + (index % 3) as f64,
                label: String::new(),
            });
        }
    }
    // Pointed at an archive that does not exist: nothing in this file may open the reader's own
    // `data/atlas.db`, and a view that tried would say so in the panel rather than read it.
    let mut view = GraphView::default().with_archive("/nonexistent/atlas-graph-interaction.db");
    view.fill(entities, edges);
    for _ in 0..240 {
        view.tick();
    }
    assert!(view.alpha <= 0.05, "the fixture's layout never came to rest");
    view.alpha = 0.0;
    view
}

// --------------------------------------------------------------------------- reading a frame

/// What one frame painted, read from its shapes.
///
/// Every field is measured from the picture: `plate` is the near-black rectangle the picture is
/// composited over, `nodes` are the filled circles in paint order, `lines` are the relation
/// segments, `texts` is every galley the frame drew, and `clips` is what each shape was clipped
/// to.
struct Painted {
    plate: Rect,
    nodes: Vec<(Pos2, f32)>,
    lines: Vec<(Pos2, Pos2)>,
    /// Every galley the frame drew: where it starts, how big it is, and what it says.
    texts: Vec<(Pos2, Vec2, String)>,
    clips: Vec<Rect>,
    /// A rectangle stroked in ink at ring weight: the canvas showing it has keyboard focus.
    rings: usize,
}

impl Default for Painted {
    fn default() -> Painted {
        Painted {
            plate: Rect::NOTHING,
            nodes: Vec::new(),
            lines: Vec::new(),
            texts: Vec::new(),
            clips: Vec::new(),
            rings: 0,
        }
    }
}

impl Painted {
    /// Every string the frame drew, in one place: "does the panel say this" is a `contains`.
    fn words(&self) -> String {
        self.texts
            .iter()
            .map(|(_, _, words)| words.as_str())
            .collect::<Vec<&str>>()
            .join("\n")
    }
}

fn painted(output: &egui::FullOutput) -> Painted {
    let mut out = Painted::default();
    for clipped in &output.shapes {
        out.clips.push(clipped.clip_rect);
        walk(&clipped.shape, &mut |shape| match shape {
            egui::Shape::Rect(rect) if rect.fill == PLATE => out.plate = rect.rect,
            egui::Shape::Rect(rect)
                if rect.fill == Color32::TRANSPARENT
                    && rect.stroke.color == theme::INK
                    && rect.stroke.width >= 1.4 =>
            {
                out.rings += 1;
            }
            egui::Shape::Circle(circle) if circle.fill == NODE => {
                out.nodes.push((circle.center, circle.radius));
            }
            egui::Shape::LineSegment { points, .. } => out.lines.push((points[0], points[1])),
            egui::Shape::Text(text) => out.texts.push((
                text.pos,
                text.galley.size(),
                text.galley.text().to_string(),
            )),
            _ => {}
        });
    }
    out
}

/// Visit a shape and everything it nests (a `Shape::Vec` is a group of shapes).
fn walk(shape: &egui::Shape, visit: &mut impl FnMut(&egui::Shape)) {
    visit(shape);
    if let egui::Shape::Vec(children) = shape {
        for child in children {
            walk(child, visit);
        }
    }
}

/// Every coordinate a frame carries, so "is anything NaN" is answerable for the whole frame.
fn coordinates(shape: &egui::Shape, out: &mut Vec<(&'static str, f32)>) {
    match shape {
        egui::Shape::Rect(rect) => {
            out.extend([
                ("rect.min.x", rect.rect.min.x),
                ("rect.min.y", rect.rect.min.y),
                ("rect.max.x", rect.rect.max.x),
                ("rect.max.y", rect.rect.max.y),
            ]);
        }
        egui::Shape::Circle(circle) => {
            out.extend([
                ("circle.x", circle.center.x),
                ("circle.y", circle.center.y),
                ("circle.radius", circle.radius),
            ]);
        }
        egui::Shape::LineSegment { points, .. } => {
            for point in points {
                out.extend([("line.x", point.x), ("line.y", point.y)]);
            }
        }
        egui::Shape::Text(text) => {
            out.extend([("text.x", text.pos.x), ("text.y", text.pos.y)]);
        }
        egui::Shape::Mesh(mesh) => {
            for vertex in &mesh.vertices {
                out.extend([("mesh.x", vertex.pos.x), ("mesh.y", vertex.pos.y)]);
            }
        }
        egui::Shape::Path(path) => {
            for point in &path.points {
                out.extend([("path.x", point.x), ("path.y", point.y)]);
            }
        }
        egui::Shape::Vec(children) => {
            for child in children {
                coordinates(child, out);
            }
        }
        _ => {}
    }
}

// --------------------------------------------------------------------------- the harness

/// A headless page: a context, a clock, and a canvas size.
///
/// The clock is explicit because egui's scroll smoothing and its double-click detection are both
/// timed: a frame that advanced no time would be a frame the wheel never finishes in, and two
/// clicks that arrived at the same instant would never be a double click.
struct Page {
    ctx: egui::Context,
    time: f64,
    size: Vec2,
}

impl Page {
    fn new(width: f32, height: f32) -> Page {
        let page = Page {
            ctx: egui::Context::default(),
            time: 0.0,
            size: Vec2::new(width, height),
        };
        theme::install(&page.ctx);
        page
    }

    /// A frame that has no time in it yet: the first one, so the clock starts before the gesture.
    fn input(&mut self, events: Vec<egui::Event>) -> egui::RawInput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        // 60 Hz: what the window's own loop asks for (`app::Atlas` requests 16 ms repaints).
        self.time += 1.0 / 60.0;
        input
    }

    /// Draw the canvas, one frame, with these events.
    fn draw(&mut self, events: Vec<egui::Event>, view: &mut GraphView) -> Painted {
        let height = self.size.y;
        let input = self.input(events);
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    view.show(ui, height);
                });
        });
        let out = painted(&output);
        if let Some(problem) = bad_coordinate(&output) {
            panic!("{problem}");
        }
        output.drop_without_applying_deltas();
        out
    }

    /// Draw the page the way `app::Atlas` draws the Graph page: a horizontal row holding the
    /// canvas, and the relation panel beside it in a column of its own width.
    ///
    /// The shape matters: a child `ui` inherits its parent's layout, so a panel built inside this
    /// row is handed a *horizontal* ui and lays its rows out side by side unless it says
    /// otherwise. Only this frame can see that, so only this frame can hold the page to it.
    fn page(&mut self, view: &mut GraphView) -> Painted {
        let height = self.size.y;
        let input = self.input(Vec::new());
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui(Vec2::new(self.size.x - 340.0, height), |ui| {
                            view.show(ui, height);
                        });
                        ui.allocate_ui(Vec2::new(320.0, height), |ui| {
                            panel(ui, view);
                        });
                    });
                });
        });
        let out = painted(&output);
        if let Some(problem) = bad_coordinate(&output) {
            panic!("{problem}");
        }
        output.drop_without_applying_deltas();
        out
    }

    /// Draw the relation panel alone, so its text can be read without the canvas's own labels.
    fn panel(&mut self, view: &GraphView) -> Painted {
        let input = self.input(Vec::new());
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.set_max_width(320.0);
                    panel(ui, view);
                });
        });
        let out = painted(&output);
        output.drop_without_applying_deltas();
        out
    }

    /// Draw frames until the camera stops moving.
    ///
    /// egui smooths a wheel event over the frames that follow it, so "the zoom is done" is a
    /// fact about the clock and not about the last event: a test that measured a click straight
    /// after a wheel would blame the click for the wheel's tail.
    fn settle(&mut self, view: &mut GraphView) {
        let mut last = (view.pan, view.zoom);
        let mut still = 0;
        for _ in 0..90 {
            self.draw(Vec::new(), view);
            let now = (view.pan, view.zoom);
            if (now.0 - last.0).length() < 0.001 && (now.1 - last.1).abs() < 0.0001 {
                still += 1;
                if still >= 2 {
                    return;
                }
            } else {
                still = 0;
            }
            last = now;
        }
        panic!("the camera never came to rest: it was still moving at {:?}", last);
    }

    /// Press and release at one point: the gesture a reader makes to select.
    fn click(&mut self, at: Pos2, view: &mut GraphView) -> Painted {
        self.draw(vec![egui::Event::PointerMoved(at)], view);
        self.draw(button(at, true), view);
        self.draw(button(at, false), view)
    }

    /// Two clicks at one point, a tenth of a second apart: the gesture a reader makes to undo a
    /// pan or a zoom.
    fn double_click(&mut self, at: Pos2, view: &mut GraphView) -> Painted {
        self.draw(vec![egui::Event::PointerMoved(at)], view);
        for _ in 0..2 {
            self.draw(button(at, true), view);
            self.draw(button(at, false), view);
        }
        self.draw(vec![egui::Event::PointerMoved(at)], view)
    }

    /// Press, move by `by` in one go, release: one drag, on a node or on the plate.
    fn drag(&mut self, from: Pos2, by: Vec2, view: &mut GraphView) -> Painted {
        self.draw(vec![egui::Event::PointerMoved(from)], view);
        self.draw(button(from, true), view);
        self.draw(vec![egui::Event::PointerMoved(from + by)], view);
        self.draw(button(from + by, false), view)
    }

    /// One wheel step over the canvas at `at`.
    fn wheel(&mut self, at: Pos2, scroll: f32, view: &mut GraphView) -> Painted {
        self.draw(vec![egui::Event::PointerMoved(at)], view);
        self.draw(
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, scroll),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            view,
        )
    }

    /// One key, pressed and released.
    fn key(&mut self, key: Key, view: &mut GraphView) -> Painted {
        self.draw(
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            view,
        )
    }
}

fn button(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }]
}

fn bad_coordinate(output: &egui::FullOutput) -> Option<String> {
    for clipped in &output.shapes {
        let mut all: Vec<(&'static str, f32)> = Vec::new();
        coordinates(&clipped.shape, &mut all);
        for (what, value) in all {
            if !value.is_finite() {
                return Some(format!(
                    "the frame painted {what} = {value}: a shape with no finite position is a \
                     shape nobody can find"
                ));
            }
        }
    }
    None
}

// --------------------------------------------------------------------------- the camera

/// Where the view says a node is painted: the published camera, not a private one.
///
/// `nodes` are layout positions, `zoom` scales them, and `pan` moves the plate under them about
/// its middle. A view whose painted circles do not sit here is a view whose picture and whose
/// state disagree, which [`the_picture_is_where_the_view_says_it_is`] states on its own.
fn projected(plate: Rect, view: &GraphView, slot: usize) -> Pos2 {
    let centre = plate.center() + view.pan;
    let node = view.nodes[slot];
    Pos2::new(centre.x + node.x * view.zoom, centre.y + node.y * view.zoom)
}

/// The node a painted circle belongs to: the one whose projection is nearest.
fn slot_at(plate: Rect, view: &GraphView, at: Pos2) -> usize {
    let mut best = (usize::MAX, f32::INFINITY);
    for slot in 0..view.nodes.len() {
        let distance = projected(plate, view, slot).distance(at);
        if distance < best.1 {
            best = (slot, distance);
        }
    }
    assert!(
        best.1 <= 0.5,
        "the circle painted at {at:?} is {:.1} px from the nearest node's projected position: \
         the picture is not where the view says the nodes are",
        best.1
    );
    assert_ne!(best.0, usize::MAX, "the graph has no nodes to aim at");
    best.0
}

/// The slots whose projection is inside the canvas, strongest first.
fn inside(plate: Rect, view: &GraphView) -> Vec<usize> {
    (0..view.nodes.len())
        .filter(|slot| plate.contains(projected(plate, view, *slot)))
        .collect()
}

// --------------------------------------------------------------------------- the harness, proved

/// The two gestures egui has to recognise — a double click and a Tab — are recognised.
///
/// Both are timed or remembered by egui itself, so a specification built on them is only as good
/// as the events behind it. This draws a plain button on the same harness and asserts that the
/// gesture arrives: if this fails, every "double click" or "Tab" below is suspect and nothing
/// else in this file means what it says.
#[test]
fn the_gestures_this_file_sends_are_the_gestures_a_reader_makes() {
    let mut page = Page::new(320.0, 160.0);
    let rect = Rect::from_min_size(Pos2::new(40.0, 20.0), Vec2::new(200.0, 60.0));
    let (mut clicked, mut doubled, mut focused) = (0, 0, 0);
    let mut frame = |page: &mut Page, events: Vec<egui::Event>| {
        let input = page.input(events);
        let output = page.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let response = ui.allocate_rect(rect, egui::Sense::click());
                    if response.clicked() {
                        clicked += 1;
                    }
                    if response.double_clicked() {
                        doubled += 1;
                    }
                    focused = usize::from(response.has_focus());
                });
        });
        output.drop_without_applying_deltas();
    };
    let at = rect.center();
    frame(&mut page, vec![egui::Event::PointerMoved(at)]);
    for _ in 0..2 {
        frame(&mut page, button(at, true));
        frame(&mut page, button(at, false));
    }
    assert!(clicked >= 2, "two press/release pairs were {clicked} clicks");
    assert_eq!(
        doubled, 1,
        "two clicks a tenth of a second apart were not one double click ({doubled})"
    );

    let mut page = Page::new(320.0, 160.0);
    let mut focusable_ui = |page: &mut Page, events: Vec<egui::Event>| {
        let input = page.input(events);
        let output = page.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let response = ui.allocate_rect(rect, egui::Sense::click());
                    focused = usize::from(response.has_focus());
                });
        });
        output.drop_without_applying_deltas();
    };
    focusable_ui(&mut page, Vec::new());
    focusable_ui(
        &mut page,
        vec![egui::Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    focusable_ui(&mut page, Vec::new());
    assert_eq!(
        focused, 1,
        "Tab did not focus the one focusable widget on the page, so no Tab test below means \
         anything"
    );
}

/// The picture is where the view says it is: every painted circle is a node, at its projected
/// position, and every node inside the canvas is painted.
#[test]
fn the_picture_is_where_the_view_says_it_is() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let drawn = page.draw(Vec::new(), &mut view);
    assert!(
        drawn.plate.width() > 600.0 && drawn.plate.height() > 440.0,
        "the canvas is {:?}, not the page it was given",
        drawn.plate.size()
    );
    let expected = inside(drawn.plate, &view);
    assert!(
        !expected.is_empty(),
        "no node is inside the canvas at the opening view: nothing below could be clicked"
    );
    assert_eq!(
        drawn.nodes.len(),
        expected.len(),
        "{} node circles were painted for {} nodes inside the canvas",
        drawn.nodes.len(),
        expected.len()
    );
    for (centre, radius) in &drawn.nodes {
        let slot = slot_at(drawn.plate, &view, *centre);
        assert!(
            *radius > 0.0,
            "node {slot} was painted with radius {radius}, which is not a circle"
        );
    }
    for slot in expected {
        assert!(
            drawn
                .nodes
                .iter()
                .any(|(centre, _)| centre.distance(projected(drawn.plate, &view, slot)) <= 0.5),
            "node {slot} ({}) is inside the canvas but was not painted",
            view.name_of(slot)
        );
    }
    // The relations are drawn between the circles they join, and every line drawn answers for a
    // relation the view holds.
    //
    // Not one line per relation: the plate prunes by weight, keeping a node's strongest
    // relations (`views::graph`'s ranked cap), so this fixture's 20 relations paint 12 lines
    // and the live archive's 900 paint 83 (`tests/graph_blob.rs` measures that). Pinning the
    // count to `edges.len()` pinned the page from before that pruning existed. What a reader
    // must be able to trust is that no line is decoration and no line ends at the wrong dot.
    assert!(
        !drawn.lines.is_empty(),
        "no relation line was painted for {} relations: the picture shows dots only",
        view.edges.len()
    );
    assert!(
        drawn.lines.len() <= view.edges.len(),
        "{} relation lines were painted for {} relations",
        drawn.lines.len(),
        view.edges.len()
    );
    let mut answered: Vec<(usize, usize)> = Vec::new();
    for (index, (a, b)) in drawn.lines.iter().enumerate() {
        for end in [a, b] {
            assert!(
                (0..view.nodes.len())
                    .any(|slot| projected(drawn.plate, &view, slot).distance(*end) <= 0.5),
                "relation line {index} ends at {end:?}, which is not a painted node's centre"
            );
        }
        let middle = *a + (*b - *a) * 0.5;
        let pair = view.pick_link(middle).unwrap_or_else(|| {
            panic!("relation line {index} at {middle:?} answers no relation: a line a reader can see and not open")
        });
        assert!(
            view.edges
                .iter()
                .any(|edge| (edge.a.min(edge.b), edge.a.max(edge.b)) == pair),
            "relation line {index} answers {pair:?}, which is not an edge of the view"
        );
        answered.push(pair);
    }
    answered.sort_unstable();
    answered.dedup();
    assert_eq!(
        answered.len(),
        drawn.lines.len(),
        "{} lines answer for {} relations: two lines are drawn for one relation",
        drawn.lines.len(),
        answered.len()
    );
}

// ------------------------------------------------------------------ the page, not the canvas

/// The panel is a column of rows beside the canvas, in the shape the page actually draws it.
///
/// This is the one claim only a page-shaped frame can make. `app::Atlas` shows the canvas and the
/// panel inside one `ui.horizontal_top`, and a child `ui` inherits its parent's layout: a panel
/// that does not say "top down" is handed a horizontal ui and lays its header, its rows and its
/// stories out side by side — one line, hundreds of pixels wide, running over the canvas. The
/// page then looks like a strip of text rather than a list, which is exactly the kind of
/// "everything is broken" a reader reports without being able to name it.
#[test]
fn the_panel_is_a_column_of_rows_beside_the_canvas() {
    let mut page = Page::new(900.0, 620.0);
    let mut view = graph(10);
    // Select a node through the canvas, the way a reader does, then draw the whole page.
    let drawn = page.draw(Vec::new(), &mut view);
    let (centre, _) = drawn.nodes[0];
    page.click(centre, &mut view);
    let selected = view.selected.expect("the setup selected no node");
    let shown = page.page(&mut view);

    let canvas_right = shown.plate.right();
    assert!(
        canvas_right > 400.0 && canvas_right < 900.0,
        "the canvas is {canvas_right} px wide in a 900 px page: the panel is not beside it"
    );
    let mut panel: Vec<(Pos2, Vec2, String)> = shown
        .texts
        .iter()
        .filter(|(at, _, _)| at.x >= canvas_right - 0.5)
        .cloned()
        .collect();
    panel.sort_by(|left, right| left.0.y.total_cmp(&right.0.y));
    assert!(
        panel.len() >= 3,
        "the panel drew {} line(s) ({}), so it is not a list of rows",
        panel.len(),
        panel
            .iter()
            .map(|(_, _, words)| words.as_str())
            .collect::<Vec<&str>>()
            .join(" | ")
    );

    // Stacked, not side by side: every line starts a little below the one before it, and all of
    // them start at the same left edge — the edge of the panel's own column.
    let left = panel[0].0.x;
    for (index, (at, size, words)) in panel.iter().enumerate() {
        assert!(
            (at.x - left).abs() <= 40.0,
            "panel line {index} ({words:?}) starts at x={}, {:.0} px from the column's left edge \
             at x={left}: the rows are not in one column",
            at.x,
            (at.x - left).abs()
        );
        assert!(
            at.x + size.x <= canvas_right + 320.0 + 1.0,
            "panel line {index} ({words:?}) runs to x={:.0}, past the panel's own 320 px column \
             (which ends at {:.0}): a row wider than its column is drawn over the canvas",
            at.x + size.x,
            canvas_right + 320.0
        );
    }
    for window in panel.windows(2) {
        assert!(
            window[1].0.y > window[0].0.y + 1.0,
            "panel lines {:?} and {:?} sit on the same baseline: the rows are drawn side by side",
            window[0].2,
            window[1].2
        );
    }

    // The canvas's own labels stay on the canvas.
    for (at, size, words) in &shown.texts {
        if at.x >= canvas_right - 0.5 {
            continue;
        }
        assert!(
            at.x + size.x <= canvas_right + 0.5,
            "the canvas label {words:?} runs to x={:.0}, into the panel's column at x={canvas_right:.0}",
            at.x + size.x
        );
    }

    // And the column says what the canvas selected.
    let words = panel
        .iter()
        .map(|(_, _, words)| words.as_str())
        .collect::<Vec<&str>>()
        .join("\n");
    assert!(
        words.contains(&view.name_of(selected)),
        "the panel column does not name {}, the node the canvas selected ({words:?})",
        view.name_of(selected)
    );
}

// --------------------------------------------------------------------------- 1. a click lands

/// Click the centre of every painted node and require the click to select that node, and the
/// panel to describe it and list its relations. `what` names the view it was done in.
fn click_every_painted_node(page: &mut Page, view: &mut GraphView, what: &str) {
    page.settle(view);
    let drawn = page.draw(Vec::new(), view);
    assert!(
        drawn.nodes.len() >= 3,
        "{what}: only {} nodes are inside the canvas, so this view cannot be clicked at all",
        drawn.nodes.len()
    );
    println!(
        "{what}: {} node circles inside a {:.0}x{:.0} canvas, zoom x{:.2}, pan {:?}",
        drawn.nodes.len(),
        drawn.plate.width(),
        drawn.plate.height(),
        view.zoom,
        view.pan
    );
    let centres: Vec<Pos2> = drawn.nodes.iter().map(|(centre, _)| *centre).collect();
    for centre in centres {
        let slot = slot_at(drawn.plate, view, centre);
        let camera = (view.pan, view.zoom);
        page.click(centre, view);
        assert_eq!(
            (view.pan, view.zoom),
            camera,
            "{what}: selecting node {slot} moved the camera from {camera:?} to {:?} — a click is \
             a selection, not a pan",
            (view.pan, view.zoom)
        );
        assert_eq!(
            view.selected,
            Some(slot),
            "{what}: clicking the middle of the node painted at {centre:?} selected {:?}, but \
             that circle is node {slot} ({})",
            view.selected,
            view.name_of(slot)
        );
        let described = page.panel(view);
        let words = described.words();
        assert!(
            words.contains(&view.name_of(slot)),
            "{what}: the panel does not name {}, the node the click selected",
            view.name_of(slot)
        );
        let relations = view.around(slot);
        assert!(
            !relations.is_empty(),
            "{what}: node {slot} has no relations in this fixture, so the panel has nothing to list"
        );
        for relation in &relations {
            assert!(
                words.contains(&relation.name),
                "{what}: the panel does not list {}, a relation of the selected node",
                relation.name
            );
        }
    }
}

/// A click on the empty plate clears the selection, so the panel goes back to saying that
/// nothing is chosen instead of describing the node the reader has just clicked away from.
#[test]
fn a_click_on_the_empty_plate_clears_the_selection() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let drawn = page.draw(Vec::new(), &mut view);
    page.click(drawn.nodes[0].0, &mut view);
    assert!(view.selected.is_some(), "the setup selected no node");
    let after = page.draw(Vec::new(), &mut view);
    let empty = empty_point(&after, 50.0);
    page.click(empty, &mut view);
    assert_eq!(
        view.selected, None,
        "clicking the empty plate at {empty:?} left {:?} selected",
        view.selected
    );
    let described = page.panel(&view);
    assert!(
        !described.words().contains("Entity "),
        "the panel still describes a node after the selection was cleared: {:?}",
        described.words()
    );
}

/// A click lands on the node under the pointer, at the zoom the page opens at.
#[test]
fn a_click_selects_the_node_it_lands_on() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    assert_eq!(view.zoom, 1.0, "the page no longer opens at ×1");
    click_every_painted_node(&mut page, &mut view, "at ×1");
}

/// A click lands on the node under the pointer at ×4, where every circle is four times as far
/// from the middle and four times as big.
#[test]
fn a_click_selects_the_node_it_lands_on_at_four_times_zoom() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let middle = page.draw(Vec::new(), &mut view).plate.center();
    let mut steps = 0;
    while view.zoom < 3.9 && steps < 40 {
        page.wheel(middle, 120.0, &mut view);
        steps += 1;
    }
    assert!(
        view.zoom >= 3.9,
        "the wheel reached only ×{:.2} in {steps} steps, so ×4 was never tested",
        view.zoom
    );
    click_every_painted_node(&mut page, &mut view, "at ×4");
}

/// A click lands on the node under the pointer after the plate has been panned 300 px in both
/// axes: a pan must not leave the click mapped from the old camera.
#[test]
fn a_click_selects_the_node_it_lands_on_after_a_pan() {
    let mut page = Page::new(900.0, 700.0);
    let mut view = graph(10);
    let before = page.draw(Vec::new(), &mut view);
    // An empty point of the plate: far from every painted circle, so the drag pans.
    let empty = empty_point(&before, 60.0);
    // The camera is press-relative and egui swallows the first pixels of a drag before
    // `drag_started()` fires, so a 300 px drag answers 304.5 px across and 285.9 px down on
    // this page — the same shortfall `views::graph`'s own comment states for 120 and 90
    // (`src/views/graph.rs:1536-1545`). The setup only has to put the camera somewhere that is
    // not the opening view, so it holds the pan to the hand's travel within that swallow; the
    // click mapping below is then exact at whatever camera the page chose.
    const SWALLOW: f32 = 16.0;
    let by = Vec2::new(300.0, 300.0);
    page.drag(empty, by, &mut view);
    assert!(
        view.pan.x > 0.0 && view.pan.y > 0.0,
        "the setup did not pan towards {by:?}: the camera sits at {:?}",
        view.pan
    );
    assert!(
        (view.pan - by).length() <= SWALLOW,
        "the setup panned {:?} for a drag of {by:?}: {:.1} px from the pointer's travel, further \
         than the {SWALLOW:.0} px egui swallows before a drag is recognised",
        view.pan,
        (view.pan - by).length()
    );
    click_every_painted_node(&mut page, &mut view, "after a 300 px pan");
}

/// A click lands on the node under the pointer after both a zoom and a pan: the two are applied
/// by the same camera, and a bug that cancels one against the other only shows when both have
/// happened.
#[test]
fn a_click_selects_the_node_it_lands_on_after_a_zoom_and_a_pan() {
    let mut page = Page::new(900.0, 700.0);
    let mut view = graph(10);
    let middle = page.draw(Vec::new(), &mut view).plate.center();
    for _ in 0..3 {
        page.wheel(middle, 120.0, &mut view);
    }
    page.settle(&mut view);
    let before = page.draw(Vec::new(), &mut view);
    let empty = empty_point(&before, 60.0);
    page.drag(empty, Vec2::new(-180.0, 140.0), &mut view);
    assert!(
        view.zoom > 1.0 && view.pan.length() > 100.0,
        "the setup reached zoom ×{:.2} and pan {:?}, which is not both",
        view.zoom,
        view.pan
    );
    click_every_painted_node(&mut page, &mut view, "after a zoom and a pan");
}

/// A point of the plate at least `clearance` px from every painted circle and inside the canvas.
fn empty_point(drawn: &Painted, clearance: f32) -> Pos2 {
    let plate = drawn.plate;
    let mut y = plate.top() + clearance;
    while y < plate.bottom() - clearance {
        let mut x = plate.left() + clearance;
        while x < plate.right() - clearance {
            let at = Pos2::new(x, y);
            if drawn
                .nodes
                .iter()
                .all(|(centre, radius)| centre.distance(at) > radius + clearance)
            {
                return at;
            }
            x += 10.0;
        }
        y += 10.0;
    }
    panic!(
        "the canvas is so full of nodes that there is no empty point {} px from all of them",
        clearance
    );
}

// --------------------------------------------------------------------------- 2. one gesture

/// A drag that starts on a node moves that node and leaves the canvas where it was.
#[test]
fn a_drag_that_starts_on_a_node_moves_that_node_and_not_the_canvas() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let before = page.draw(Vec::new(), &mut view);
    let (centre, _) = before.nodes[0];
    let slot = slot_at(before.plate, &view, centre);
    let pan = view.pan;
    let neighbours: Vec<Pos2> = (0..view.nodes.len())
        .filter(|other| *other != slot)
        .map(|other| projected(before.plate, &view, other))
        .collect();

    let by = Vec2::new(40.0, -30.0);
    let after = page.drag(centre, by, &mut view);
    // The drag wakes the layout on purpose (`drag_node`), so the frame that confirms the move
    // must not also settle: freeze it where the drag left it.
    view.alpha = 0.0;
    let frozen = page.draw(Vec::new(), &mut view);

    assert_eq!(
        view.pan, pan,
        "a drag that started on node {slot} also panned the canvas to {:?}",
        view.pan
    );
    let landed = projected(frozen.plate, &view, slot);
    assert!(
        landed.distance(centre + by) <= 2.0,
        "dragging node {slot} by {by:?} left it at {landed:?}, {:.1} px from where the pointer \
         let go ({:?})",
        landed.distance(centre + by),
        centre + by
    );
    // The frame the drag itself painted already shows the node at the pointer, and the frame
    // after it (with the layout frozen) shows it in the same place.
    for (when, drawn) in [("the frame the drag ended on", &after), ("the frame after it", &frozen)] {
        let painted_at = drawn
            .nodes
            .iter()
            .map(|(centre, _)| *centre)
            .find(|at| at.distance(landed) <= 2.0)
            .unwrap_or_else(|| {
                panic!(
                    "{when} painted no circle within 2 px of {landed:?}, where node {slot} was \
                     dragged to (circles: {:?})",
                    drawn.nodes.iter().map(|(centre, _)| *centre).collect::<Vec<Pos2>>()
                )
            });
        assert!(
            painted_at.distance(landed) <= 2.0,
            "{when}: the node moved to {landed:?} and was painted at {painted_at:?}"
        );
    }
    for (index, neighbour) in neighbours.iter().enumerate() {
        let other = if index >= slot { index + 1 } else { index };
        let moved = projected(frozen.plate, &view, other).distance(*neighbour);
        assert!(
            moved <= 6.0,
            "a drag on node {slot} moved node {other} by {moved:.1} px: the drag is pulling the \
             whole layout, not placing one node"
        );
    }
}

/// A drag on a node follows the pointer in *screen* space, at ×4 as at ×1: the node goes where
/// the hand goes, not where the layout grid goes.
#[test]
fn a_drag_on_a_node_follows_the_pointer_at_four_times_zoom() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let middle = page.draw(Vec::new(), &mut view).plate.center();
    for _ in 0..12 {
        page.wheel(middle, 120.0, &mut view);
    }
    assert!(
        view.zoom > 1.5,
        "twelve wheel steps reached only ×{:.2}, so the zoom test would be a ×1 test",
        view.zoom
    );
    page.settle(&mut view);
    let drawn = page.draw(Vec::new(), &mut view);
    assert!(
        !drawn.nodes.is_empty(),
        "at ×{:.2} no node is inside the canvas",
        view.zoom
    );
    let (centre, _) = drawn.nodes[0];
    let slot = slot_at(drawn.plate, &view, centre);
    let by = Vec2::new(40.0, -30.0);
    page.drag(centre, by, &mut view);
    view.alpha = 0.0;
    let frozen = page.draw(Vec::new(), &mut view);
    let landed = projected(frozen.plate, &view, slot);
    assert!(
        landed.distance(centre + by) <= 2.0,
        "at ×{:.2} a drag of {by:?} left node {slot} at {landed:?}, {:.1} px from the pointer: \
         the node must follow the hand, not the layout grid",
        view.zoom,
        landed.distance(centre + by)
    );
}

/// A drag that starts on empty plate pans the canvas and moves no node.
#[test]
fn a_drag_on_the_empty_plate_pans_and_moves_no_node() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let before = page.draw(Vec::new(), &mut view);
    let empty = empty_point(&before, 70.0);
    let nodes: Vec<(f32, f32)> = view.nodes.iter().map(|node| (node.x, node.y)).collect();
    let painted: Vec<Pos2> = before.nodes.iter().map(|(centre, _)| *centre).collect();

    let by = Vec2::new(-120.0, 90.0);
    let after = page.drag(empty, by, &mut view);

    for (index, (x, y)) in nodes.iter().enumerate() {
        let node = view.nodes[index];
        assert!(
            (node.x - x).abs() < 0.001 && (node.y - y).abs() < 0.001,
            "panning from {empty:?} moved node {index} from ({x}, {y}) to ({}, {})",
            node.x,
            node.y
        );
    }
    // The pan is measured from where the press landed, and egui swallows the few pixels a drag
    // needs to be recognised before `drag_started()` fires: this drag of (−120, 90) answers
    // (−115.0, 75.9), 5.0 px and 14.1 px short, the numbers `src/views/graph.rs:1536-1545`
    // records. So the camera follows the hand's travel within that swallow, per axis and in
    // the hand's direction — which is what "the plate pans" means for a reader.
    const SWALLOW: f32 = 16.0;
    assert!(
        view.pan.x < 0.0 && view.pan.y > 0.0,
        "a drag of {by:?} left the canvas at {:?}, which is not the direction of the hand",
        view.pan
    );
    assert!(
        (view.pan - by).length() <= SWALLOW,
        "a drag of {by:?} on the empty plate left the canvas at {:?}, {:.1} px from the \
         pointer's travel: further than the {SWALLOW:.0} px egui swallows before a drag is \
         recognised",
        view.pan,
        (view.pan - by).length()
    );
    // Every node that is still on the canvas moved with the plate, by exactly the pan.
    let mut compared = 0;
    for (index, was) in painted.iter().enumerate() {
        let now = projected(after.plate, &view, index);
        if !after.plate.contains(now) {
            continue;
        }
        let painted_now = after
            .nodes
            .iter()
            .map(|(centre, _)| *centre)
            .find(|at| at.distance(now) <= 0.5)
            .unwrap_or_else(|| {
                panic!("node {index} projects to {now:?} after the pan but was not painted there")
            });
        let moved = painted_now - *was;
        assert!(
            (moved - by).length() < 1.0,
            "the picture of node {index} moved {moved:?} for a pan of {by:?}: a pan that does not \
             move the whole picture by the pointer's movement is a pan the reader cannot follow"
        );
        compared += 1;
    }
    assert!(
        compared >= 3,
        "only {compared} nodes were left on the canvas after the pan, so the picture's movement \
         was barely measured"
    );
}

// --------------------------------------------------------------------------- 3. the wheel

/// The wheel zooms about the pointer: the node under it keeps its screen position, within a
/// pixel, six steps in and six steps out.
#[test]
fn the_wheel_zooms_about_the_pointer() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let drawn = page.draw(Vec::new(), &mut view);
    let (anchor, _) = drawn.nodes[0];
    let slot = slot_at(drawn.plate, &view, anchor);
    let opened = view.zoom;

    let mut worst: f32 = 0.0;
    for step in 0..6 {
        page.wheel(anchor, 120.0, &mut view);
        page.draw(vec![egui::Event::PointerMoved(anchor)], &mut view);
        let drifted = projected(drawn.plate, &view, slot).distance(anchor);
        worst = worst.max(drifted);
        assert!(
            drifted <= 1.0,
            "step {step} in: node {slot} slid {drifted:.2} px away from the pointer at \
             {anchor:?} while the wheel zoomed (zoom now ×{:.2})",
            view.zoom
        );
    }
    assert!(
        view.zoom > opened,
        "six wheel steps in took the zoom from ×{opened:.2} to ×{:.2}, which is not zooming",
        view.zoom
    );
    for step in 0..6 {
        page.wheel(anchor, -120.0, &mut view);
        page.draw(vec![egui::Event::PointerMoved(anchor)], &mut view);
        let drifted = projected(drawn.plate, &view, slot).distance(anchor);
        worst = worst.max(drifted);
        assert!(
            drifted <= 1.0,
            "step {step} out: node {slot} slid {drifted:.2} px away from the pointer at \
             {anchor:?} while the wheel zoomed out (zoom now ×{:.2})",
            view.zoom
        );
    }
    println!("wheel: the anchored node never drifted more than {worst:.2} px");
}

// --------------------------------------------------------------------------- 4. the way back

/// Push the view away until part of the graph is off the canvas, and say how far it went.
fn lose_the_graph(page: &mut Page, view: &mut GraphView) -> (usize, usize) {
    let drawn = page.draw(Vec::new(), view);
    let empty = empty_point(&drawn, 70.0);
    page.drag(empty, Vec2::new(320.0, 280.0), view);
    let pushed = page.draw(Vec::new(), view);
    let visible = inside(pushed.plate, view).len();
    let total = view.nodes.len();
    assert!(
        visible < total,
        "the setup panned the picture but every one of the {total} nodes is still inside the \
         canvas at {:?}, so 'bring them back' would be untestable",
        view.pan
    );
    (visible, total)
}

/// The requirement: a double click on the empty plate is the way back to the whole graph.
///
/// Written as a requirement because it is one: a reader who has panned the picture into a corner
/// has no other way to find the graph again, and a canvas that cannot be recovered is a canvas
/// that is lost for good.
#[test]
fn a_double_click_on_the_empty_plate_brings_the_whole_graph_back() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let (visible, total) = lose_the_graph(&mut page, &mut view);
    let drawn = page.draw(Vec::new(), &mut view);
    let empty = empty_point(&drawn, 40.0);
    page.double_click(empty, &mut view);
    let back = page.draw(Vec::new(), &mut view);
    let now = inside(back.plate, &view);
    assert_eq!(
        now.len(),
        total,
        "double-clicking the empty plate at {empty:?} left the picture where it was: {} of {total} \
         nodes are inside the canvas (it was {visible} before the double click). The page has no \
         way back to the whole graph — a double click on the plate is the gesture a reader \
         expects.",
        now.len()
    );
}

/// The requirement: Home, with the canvas focused, is the keyboard's way back to the whole graph.
#[test]
fn home_with_the_canvas_focused_brings_the_whole_graph_back() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let (visible, total) = lose_the_graph(&mut page, &mut view);
    page.key(Key::Tab, &mut view);
    let focused = page.draw(Vec::new(), &mut view);
    assert!(
        focused.rings >= 1,
        "the canvas does not show that Tab focused it: no focus ring was painted"
    );
    page.key(Key::Home, &mut view);
    let back = page.draw(Vec::new(), &mut view);
    let now = inside(back.plate, &view);
    assert_eq!(
        now.len(),
        total,
        "Home, with the canvas focused, left {} of {total} nodes inside the canvas (it was \
         {visible} before). The keyboard has no way back to the whole graph: Home is the key a \
         reader presses to go to the beginning.",
        now.len()
    );
}

// --------------------------------------------------------------------------- 5. the keyboard

/// Tab focuses the canvas and shows it; the arrows step the selection in a stable order; Esc
/// clears it; and the node left selected is the node the panel describes.
///
/// The Esc claim is the one that fails today: egui clears its focused widget on Escape before the
/// canvas is drawn, so a handler that asks `has_focus()` never runs on that frame — the selection
/// survives and the arrows stop answering.
#[test]
fn the_keyboard_walks_the_nodes_and_the_panel_follows() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);

    let unfocused = page.draw(Vec::new(), &mut view);
    assert_eq!(
        unfocused.rings, 0,
        "the canvas painted a focus ring before anything focused it"
    );
    page.key(Key::Tab, &mut view);
    let focused = page.draw(Vec::new(), &mut view);
    assert!(
        focused.rings >= 1,
        "Tab did not focus the canvas, or the canvas did not show it: no focus ring was painted"
    );

    let order = view.strongest();
    assert_eq!(order.len(), view.nodes.len(), "the strength order lost a node");
    assert_eq!(
        view.strongest(),
        order,
        "the strength order changed between two calls on a frozen graph"
    );

    page.key(Key::ArrowRight, &mut view);
    assert_eq!(
        view.selected,
        Some(order[0]),
        "the first ArrowRight selected {:?}, not the strongest node {}",
        view.selected,
        view.name_of(order[0])
    );
    page.key(Key::ArrowRight, &mut view);
    assert_eq!(
        view.selected,
        Some(order[1]),
        "the second ArrowRight selected {:?}, not the second node in the order",
        view.selected
    );
    page.key(Key::ArrowLeft, &mut view);
    assert_eq!(
        view.selected,
        Some(order[0]),
        "ArrowLeft did not come back to the node ArrowRight left"
    );
    page.key(Key::ArrowLeft, &mut view);
    assert_eq!(
        view.selected,
        Some(*order.last().unwrap_or(&0)),
        "ArrowLeft off the start of the order did not wrap to its end"
    );

    // The panel describes the node the arrows left selected — the same name, not another node's.
    let described = page.panel(&view);
    let selected = view.selected.expect("the arrows left nothing selected");
    let words = described.words();
    assert!(
        words.contains(&view.name_of(selected)),
        "the panel does not describe {}, the node the keyboard selected (it says: {:?})",
        view.name_of(selected),
        words
    );

    page.key(Key::Escape, &mut view);
    let after_escape = page.draw(Vec::new(), &mut view);
    assert_eq!(
        view.selected, None,
        "Esc left {:?} selected, so the picture keeps a selection the reader cancelled and the \
         panel keeps describing a node the keyboard has left. The Esc frame painted {} focus \
         ring(s): when that is 0, egui took the canvas's focus on the key before the canvas saw \
         it (egui-0.37.0/src/memory/mod.rs:597), so a handler gated on `has_focus()` never runs.",
        view.selected, after_escape.rings
    );
}

/// With nothing focused the arrows must not act: a canvas that steps its selection whenever an
/// arrow is pressed takes the keyboard away from whatever the reader is actually on.
#[test]
fn the_arrows_do_nothing_while_the_canvas_is_unfocused() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    page.draw(Vec::new(), &mut view);
    page.key(Key::ArrowRight, &mut view);
    assert_eq!(
        view.selected, None,
        "an arrow key selected {:?} on a canvas nobody had focused",
        view.selected
    );
    let drawn = page.draw(Vec::new(), &mut view);
    assert_eq!(
        drawn.rings, 0,
        "the canvas painted a focus ring without being focused"
    );
}

/// Tab reaches the canvas inside the window's focus cycle, and the arrows work once it has it.
///
/// The canvas is no longer the page's *only* focus stop: the camera buttons beside it are
/// stops of their own, and they paint a ring like everything else a keyboard can land on. So
/// a second Tab leaving the canvas is correct — what must not happen is a cycle the canvas is
/// missing from, which would leave a keyboard reader able to see a picture they cannot pan,
/// zoom or step through. Measured: the first Tab paints the canvas's ring (1 ring on Tab 1,
/// 0 on Tab 2, where a camera button has it), and the arrows act while it is on the canvas.
#[test]
fn tab_reaches_the_canvas_within_the_focus_cycle() {
    const CYCLE: usize = 8;
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let mut reached = None;
    for press in 1..=CYCLE {
        page.key(Key::Tab, &mut view);
        let drawn = page.draw(Vec::new(), &mut view);
        if drawn.rings >= 1 {
            reached = Some(press);
            break;
        }
    }
    let press = reached.unwrap_or_else(|| {
        panic!(
            "{CYCLE} Tabs never put the keyboard focus ring on the canvas: the plate is \
             reachable with a mouse and not with a keyboard"
        )
    });
    page.key(Key::ArrowRight, &mut view);
    assert!(
        view.selected.is_some(),
        "the canvas had focus after {press} Tab(s) and an arrow did nothing: the keys are \
         wired to something other than the focus ring"
    );
}

// --------------------------------------------------------------------------- 6. the storm

/// Four hundred events the page has to survive: mixed clicks, drags, wheels, keys and three
/// canvas sizes. Nothing painted may leave the canvas rect, and nothing may be NaN.
#[test]
fn a_storm_of_events_leaves_nothing_outside_the_canvas_and_nothing_nan() {
    let mut page = Page::new(640.0, 480.0);
    let mut view = graph(10);
    let sizes = [
        Vec2::new(320.0, 240.0),
        Vec2::new(900.0, 700.0),
        Vec2::new(640.0, 480.0),
    ];
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move |bound: f32| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let unit = ((seed >> 33) as f32) / (u32::MAX as f32 / 2.0);
        unit * bound
    };
    let mut events = 0;
    let mut worst: Option<String> = None;
    for frame in 0..400 {
        if frame % 40 == 0 {
            page.size = sizes[(frame / 40) % sizes.len()];
        }
        let at = Pos2::new(next(page.size.x), next(page.size.y));
        let batch = match frame % 8 {
            0 | 1 => vec![egui::Event::PointerMoved(at)],
            2 => button(at, true),
            3 => button(at, false),
            4 => vec![
                egui::Event::PointerMoved(at),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, next(400.0) - 200.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            5 => vec![egui::Event::Key {
                key: [Key::ArrowLeft, Key::ArrowRight, Key::Escape, Key::Home][frame % 4],
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            6 => vec![egui::Event::PointerMoved(at), egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            _ => vec![egui::Event::PointerMoved(at)],
        };
        events += batch.len();
        let height = page.size.y;
        let input = page.input(batch);
        let output = page.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    view.show(ui, height);
                });
        });
        let drawn = painted(&output);
        if worst.is_none() {
            if let Some(problem) = bad_coordinate(&output) {
                worst = Some(format!("frame {frame}: {problem}"));
            }
        }
        if worst.is_none() && !drawn.plate.is_positive() {
            worst = Some(format!(
                "frame {frame}: the canvas painted no plate rect at all (canvas {:?})",
                page.size
            ));
        }
        if worst.is_none() {
            for (index, clip) in drawn.clips.iter().enumerate() {
                if !drawn.plate.expand(0.5).contains_rect(*clip) {
                    worst = Some(format!(
                        "frame {frame}: shape {index} was clipped to {clip:?}, which is outside \
                         the canvas {:?} — something is painted beyond the canvas",
                        drawn.plate
                    ));
                    break;
                }
            }
        }
        if worst.is_none() {
            // A node whose *centre* leaves the canvas must not be drawn at all — that is the
            // page's own rule, and it is what keeps a graph panned into a corner from painting
            // rings around nothing. A circle that straddles the edge is ordinary: the clip rect
            // above is what keeps its outside half off the screen.
            for (centre, radius) in &drawn.nodes {
                if !drawn.plate.contains(*centre) {
                    worst = Some(format!(
                        "frame {frame}: a node was painted as a circle at {centre:?} r={radius} \
                         with its centre outside the canvas {:?}",
                        drawn.plate
                    ));
                    break;
                }
            }
        }
        output.drop_without_applying_deltas();
        if worst.is_some() {
            break;
        }
    }
    if let Some(problem) = worst {
        panic!("the graph broke under a storm of {events} events: {problem}");
    }
    println!(
        "{events} events, 400 frames, three canvas sizes: nothing painted outside the canvas, \
         nothing NaN"
    );
}

