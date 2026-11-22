//! Is the Graph page legible at the size it opens in? Measured, not judged.
//!
//! The reader's complaint is "ein großer Blob": 120 entities and ~900 relations in an
//! 800×500 plate. This file answers that with numbers a reviewer can check, through the
//! public API only — [`GraphView::fill`], [`GraphView::show`], [`GraphView::tick`] and
//! the shapes the frame paints. It never edits `views::graph`, and it never opens a
//! window: one headless `egui::Context`, one canvas of the page's real size.
//!
//! The five claims, each a test of its own:
//!
//! 1. **The plate is not a solid disc** — how much of the canvas the node circles cover,
//!    and how far apart neighbouring dots are.
//! 2. **No two drawn dots overlap** — a pair whose circles intersect is one dot, not two.
//! 3. **The strongest relations are readable** — of the 40 heaviest, how many run through
//!    more than [`CROSSED_NODES`] other dots.
//! 4. **The camera zooms and holds its anchor** — a pinch and a wheel both change the
//!    zoom monotonically, and the dot under the pointer does not move.
//! 5. **A hub can be isolated** — a double click on the busiest node draws its
//!    neighbourhood only, and Escape brings the whole graph back.
//!
//! # The plate
//!
//! 800×500 is the Graph page's canvas on a 1440-wide window: the page box is the window
//! less the rail and the page margins, and the plate is that box less `PANEL_WIDTH`
//! (320) and `GRAPH_GAP`. The numbers below are therefore the numbers the reader sees on
//! an ordinary laptop, not a worst case.
//!
//! # The data
//!
//! The live archive, read-only, through the same two reads `app::Atlas::load` makes:
//! [`Archive::entities`]`(72, 120)` and [`Archive::edges`]`(&ids, 900)`. A checkout
//! without an archive prints why and returns; the fixture-level claims live in
//! `graph_interaction.rs`, `graph_bounds.rs` and `graph_edges.rs`.
//!
//! Run with `--nocapture` to read every measurement:
//! `cargo test --offline --locked -p atlas-desktop --test graph_blob -- --nocapture`.

use std::path::{Path, PathBuf};

use atlas_desktop::data::{Archive, Edge, Entity};
use atlas_desktop::theme;
use atlas_desktop::views::graph::{GraphView, ALPHA_REST};
use egui::{Color32, Pos2, Rect, Vec2};

// --------------------------------------------------------------------------- thresholds
//
// Every ceiling below is where a reader stops being able to read the picture, and each
// one is a claim about the page rather than a copy of today's number. They are named and
// justified here so a change to the layout has to argue with the reason, not the integer.

/// The page's canvas on a 1440-wide window: `page_box − PANEL_WIDTH(320) − GRAPH_GAP`.
const PLATE: Vec2 = Vec2::new(800.0, 500.0);

/// How much of the plate may be covered by node ink before the dots read as one shape.
///
/// A quarter of this plate is 100 000 px² of ink: across 120 nodes that is an average disc
/// of radius 16 px, so *every* node would be drawn at an actor's size and the clear space
/// between the marks would be gone. That is the reader's "großer Blob" in one number, and
/// the page has no reason to come near it while it stays ranked.
const COVERAGE_CEILING: f32 = 0.25;

/// The median distance between neighbouring dot centres, in px, under which names cannot
/// be placed.
///
/// A label is 11 px tall and sits 4 px (`LABEL_GAP` in `views::graph`) beside its dot, so
/// 16 px is the least a named dot needs on one side before the next dot's name is on top
/// of it. Below this the plate can only be read as a texture, never as a graph of names.
const NEIGHBOUR_FLOOR: f32 = 16.0;

/// Overlapping dot pairs allowed. Two circles that intersect are drawn as one blob, and a
/// picture where that happens has lost the node it happened to: zero is the only ceiling
/// a reader can defend.
const OVERLAP_CEILING: usize = 0;

/// The heaviest relations the eye follows first, and how many of them may be buried.
///
/// Ten per cent of the strongest forty (four) is the most that can be lost without the
/// page lying about what the archive says: a line through a dot reads as a line to that
/// dot.
const STRONGEST: usize = 40;
const CROWDED_CEILING: usize = 4;

/// A relation is "buried" when its line passes through more than this many other dots.
const CROSSED_NODES: usize = 3;

/// The drawn radius at which a dot counts as an obstacle to a relation line, in px.
///
/// The widest a relation is stroked is 5 px (`WIDTH_MAX`), so a dot has to be wider than
/// that — 8 px across, a radius of 4 — before it can hide the line under it. Below that the dot is
/// dust on the line: the eye follows the relation straight through it. Counted without
/// this floor, every stray grain would be scored as a node the line might end at, and the
/// measurement would stop describing what the reader sees.
const OBSTACLE_RADIUS: f32 = 4.0;

// --------------------------------------------------------------------------- the archive

/// The checkout's archive: `rust/atlas-desktop` sits two directories below the root.
fn archive_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/atlas.db")
}

/// The live archive, or `None` on a checkout without one, with a line saying so.
fn live() -> Option<(Archive, PathBuf)> {
    let path = archive_path();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some((archive, path)),
        Err(problem) => {
            println!("skipped: {problem}");
            None
        }
    }
}

/// The graph the page loads, at rest: the window's own reads, the layout run to the floor
/// the page stops at.
fn page_of(archive: &Archive, path: &Path) -> (GraphView, Vec<Entity>, Vec<Edge>) {
    let entities = archive.entities(72, 120);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let edges = archive.edges(&ids, 900);
    let mut view = GraphView::default().with_archive(path);
    view.fill(entities.clone(), edges.clone());
    for _ in 0..4_000 {
        view.tick();
        if view.alpha <= ALPHA_REST {
            break;
        }
    }
    assert!(
        view.alpha <= ALPHA_REST,
        "the layout never came to rest (alpha {} after 4 000 ticks), so nothing here is \
         measurable",
        view.alpha
    );
    (view, entities, edges)
}

// --------------------------------------------------------------------------- reading a frame

/// What one frame painted, as far as this file needs it: the plate, every dot with the
/// radius it was drawn at, and the count of filled dot circles.
struct Painted {
    plate: Rect,
    dots: Vec<(Pos2, f32)>,
    /// Relation lines the frame painted, by endpoint: the page keeps only the strongest
    /// few per node, so this is how much of the archive's relation set is on the plate.
    lines: Vec<(Pos2, Pos2)>,
}

impl Default for Painted {
    fn default() -> Painted {
        Painted {
            plate: Rect::NOTHING,
            dots: Vec::new(),
            lines: Vec::new(),
        }
    }
}

fn walk(shape: &egui::Shape, visit: &mut impl FnMut(&egui::Shape)) {
    visit(shape);
    if let egui::Shape::Vec(children) = shape {
        for child in children {
            walk(child, visit);
        }
    }
}

fn painted(output: &egui::FullOutput) -> Painted {
    // The page's own palette values, not copies: `views::graph` paints the plate with
    // `theme::RECESS` and every node with `theme::INK`, and a reader that hard-coded them
    // would silently measure nothing the day the palette moved.
    const PLATE_INK: Color32 = theme::RECESS;
    const DOT_INK: Color32 = theme::INK;
    let mut out = Painted::default();

    for clipped in &output.shapes {
        walk(&clipped.shape, &mut |shape| match shape {
            egui::Shape::Rect(rect) if rect.fill == PLATE_INK => out.plate = rect.rect,
            egui::Shape::Circle(circle) if circle.fill == DOT_INK => {
                out.dots.push((circle.center, circle.radius))
            }
            egui::Shape::LineSegment { points, .. } => out.lines.push((points[0], points[1])),
            _ => {}
        });
    }
    out
}

// --------------------------------------------------------------------------- the harness

/// A headless canvas: one context, an explicit clock, the page's plate size.
struct Plate {
    ctx: egui::Context,
    time: f64,
    size: Vec2,
    /// The zoom factor egui reported to the page during the last frame: proof that a
    /// gesture reached the widget, so a red assertion is about the page and not the harness.
    delivered: f32,
}

impl Plate {
    fn new() -> Plate {
        let plate = Plate {
            ctx: egui::Context::default(),
            time: 0.0,
            size: PLATE,
            delivered: 1.0,
        };
        theme::install(&plate.ctx);
        plate
    }

    fn input(&mut self, events: Vec<egui::Event>) -> egui::RawInput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        input
    }

    /// One frame of the canvas, at exactly the page's plate size.
    fn frame(&mut self, events: Vec<egui::Event>, view: &mut GraphView) -> Painted {
        let height = self.size.y;
        let input = self.input(events);
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.allocate_ui(self.size, |ui| {
                        view.show(ui, height);
                        self.delivered = ui.ctx().input(|input| input.zoom_delta());
                    });
                });
        });
        let out = painted(&output);
        output.drop_without_applying_deltas();
        out
    }

    /// Draw until the camera and the layout stop moving, so a measurement is of a picture
    /// at rest rather than of a frame in the middle of a gesture.
    fn settle(&mut self, view: &mut GraphView) -> Painted {
        let mut last = (view.pan, view.zoom, view.alpha);
        let mut still = 0;
        let mut out;
        for _ in 0..240 {
            out = self.frame(Vec::new(), view);
            let now = (view.pan, view.zoom, view.alpha);
            if (now.0 - last.0).length() < 0.001
                && (now.1 - last.1).abs() < 0.0001
                && (now.2 - last.2).abs() < 0.0001
            {
                still += 1;
                if still >= 3 {
                    return out;
                }
            } else {
                still = 0;
            }
            last = now;
        }
        panic!("the picture never came to rest: still moving at {last:?}");
    }

    /// Two presses a fifth of a second apart on the same point: a double click.
    fn double_click(&mut self, at: Pos2, view: &mut GraphView) {
        self.frame(vec![egui::Event::PointerMoved(at)], view);
        for _ in 0..2 {
            self.frame(button(at, true), view);
            self.frame(button(at, false), view);
            self.time += 0.2;
        }
    }

    fn key(&mut self, key: egui::Key, view: &mut GraphView) {
        self.frame(
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            view,
        );
        self.frame(
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            view,
        );
    }

    /// A pinch: the gesture a trackpad sends, read by `i.zoom_delta()`. Asserts the input
    /// state saw the factor, so a page that does not zoom is the page's fault, not the
    /// harness's.
    fn pinch(&mut self, at: Pos2, factor: f32, view: &mut GraphView) {
        self.frame(vec![egui::Event::PointerMoved(at)], view);
        self.frame(vec![egui::Event::Zoom(factor)], view);
        assert!(
            (self.delivered - factor).abs() < 1.0e-3,
            "the harness delivered a zoom factor of {} instead of {factor}: egui did not pass \
             the pinch to the widget",
            self.delivered
        );
    }

    /// One wheel step over the canvas at `at`.
    fn wheel(&mut self, at: Pos2, step: f32, view: &mut GraphView) {
        self.frame(vec![egui::Event::PointerMoved(at)], view);
        self.frame(
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, step),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            }],
            view,
        );
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

// --------------------------------------------------------------------------- geometry

/// Where the view says a node is painted: published camera, published layout.
fn projected(plate: Rect, view: &GraphView, slot: usize) -> Pos2 {
    let centre = plate.center() + view.pan;
    let node = view.nodes[slot];
    Pos2::new(centre.x + node.x * view.zoom, centre.y + node.y * view.zoom)
}

fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

/// The distance from a point to a segment: how far a line passes from a dot's centre.
fn segment_distance(point: Pos2, one: Pos2, two: Pos2) -> f32 {
    let span = two - one;
    let length = span.length_sq();
    if length <= f32::EPSILON {
        return point.distance(one);
    }
    let t = ((point - one).dot(span) / length).clamp(0.0, 1.0);
    point.distance(one + span * t)
}

// --------------------------------------------------------------------------- 1. the disc

/// The plate is not a solid disc: node ink stays under a readable share of it, and the
/// median gap between neighbouring dots leaves room for a name.
#[test]
fn the_plate_is_not_a_solid_disc() {
    let Some((archive, path)) = live() else { return };
    let (mut view, entities, edges) = page_of(&archive, &path);
    let mut plate = Plate::new();
    let frame = plate.settle(&mut view);
    eprintln!(
        "blob: the page's own reads are {} entities and {} relations",
        entities.len(),
        edges.len()
    );
    let area = frame.plate.width() * frame.plate.height();
    assert!(area > 0.0, "the frame painted no plate");

    let ink: f32 = frame
        .dots
        .iter()
        .map(|(_, radius)| std::f32::consts::PI * radius * radius)
        .sum();
    let coverage = ink / area;
    let mut gaps: Vec<f32> = Vec::with_capacity(frame.dots.len());
    for (index, (dot, _)) in frame.dots.iter().enumerate() {
        let nearest = frame
            .dots
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, (at, _))| at.distance(*dot))
            .fold(f32::INFINITY, f32::min);
        if nearest.is_finite() {
            gaps.push(nearest);
        }
    }
    let median_gap = median(&mut gaps);
    let mut radii: Vec<f32> = frame.dots.iter().map(|(_, radius)| *radius).collect();
    radii.sort_by(f32::total_cmp);
    eprintln!(
        "blob: {} dots cover {:.1} % of the {:?} plate (fit zoom {:.3}); median nearest \
         neighbour {:.2} px",
        frame.dots.len(),
        coverage * 100.0,
        frame.plate.size(),
        view.zoom,
        median_gap
    );
    eprintln!(
        "blob: drawn dot radii {:.1} / {:.1} / {:.1} px (min / median / max); the frame paints \
         {} of the {} relations",
        radii.first().copied().unwrap_or(0.0),
        median(&mut radii),
        radii.last().copied().unwrap_or(0.0),
        frame.lines.len(),
        view.edges.len()
    );

    assert!(
        coverage <= COVERAGE_CEILING,
        "node ink covers {:.1} % of the plate (ceiling {:.0} %): at that density the dots \
         read as one shape",
        coverage * 100.0,
        COVERAGE_CEILING * 100.0
    );
    assert!(
        median_gap >= NEIGHBOUR_FLOOR,
        "the median nearest neighbour is {:.1} px apart (floor {:.0} px): no name fits \
         between them",
        median_gap,
        NEIGHBOUR_FLOOR
    );
}

// --------------------------------------------------------------------------- 2. overlap

/// No two drawn dots overlap: an intersecting pair is one dot on the screen.
#[test]
fn no_two_drawn_nodes_overlap() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = page_of(&archive, &path);
    let mut plate = Plate::new();
    let frame = plate.settle(&mut view);
    // The opening view draws the ranked set (at most `ACTORS_AT_FIT` = 60 entities), not every
    // node in the archive: what must not overlap is what is *painted*.
    assert!(
        !frame.dots.is_empty() && frame.dots.len() <= 60,
        "the frame paints {} of {} nodes: not the ranked opening picture there is to check",
        frame.dots.len(),
        view.nodes.len()
    );
    let mut overlapping: Vec<(usize, usize, f32, f32)> = Vec::new();
    for (index, (one, radius_one)) in frame.dots.iter().enumerate() {
        for (other, (two, radius_two)) in frame.dots.iter().enumerate().skip(index + 1) {
            let distance = one.distance(*two);
            if radius_one + radius_two > distance {
                overlapping.push((index, other, radius_one + radius_two, distance));
            }
        }
    }
    eprintln!(
        "blob: {} of {} dot pairs overlap (deepest: {:.1} px of radii in {:.1} px of space)",
        overlapping.len(),
        frame.dots.len() * frame.dots.len().saturating_sub(1) / 2,
        overlapping
            .iter()
            .map(|(_, _, radii, _)| *radii)
            .fold(0.0, f32::max),
        overlapping
            .iter()
            .map(|(_, _, _, distance)| *distance)
            .fold(f32::INFINITY, f32::min)
    );
    assert!(
        overlapping.len() <= OVERLAP_CEILING,
        "{} pairs of drawn dots intersect (ceiling {OVERLAP_CEILING}); the first is {:?}",
        overlapping.len(),
        overlapping.first()
    );
}

// --------------------------------------------------------------------------- 3. legibility

/// The strongest relations are readable: of the heaviest [`STRONGEST`], at most
/// [`CROWDED_CEILING`] run through more than [`CROSSED_NODES`] other dots.
#[test]
fn the_strongest_relations_are_legible() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = page_of(&archive, &path);
    let mut plate = Plate::new();
    let frame = plate.settle(&mut view);
    let plate_box = frame.plate;

    // The heaviest relations, by the stored weight the page ranks on.
    let mut heavy: Vec<usize> = (0..view.edges.len()).collect();
    heavy.sort_by(|left, right| {
        view.edges[*right]
            .stories
            .total_cmp(&view.edges[*left].stories)
            .then_with(|| left.cmp(right))
    });
    heavy.truncate(STRONGEST);

    let mut crowded = 0usize;
    let mut worst = 0usize;
    let mut painted = 0usize;
    for index in &heavy {
        let link = &view.edges[*index];
        let (one, two) = (
            projected(plate_box, &view, link.a),
            projected(plate_box, &view, link.b),
        );
        if frame.lines.iter().any(|(start, end)| {
            (start.distance(one) < 1.5 && end.distance(two) < 1.5)
                || (start.distance(two) < 1.5 && end.distance(one) < 1.5)
        }) {
            painted += 1;
        }
        let mut crossed = 0usize;
        for (slot, _node) in view.nodes.iter().enumerate() {
            if slot == link.a || slot == link.b {
                continue;
            }
            let at = projected(plate_box, &view, slot);
            // The radius the dot is drawn at on this plate, measured: a grain is 2.6 px and
            // an actor up to 27 px, and a line through either is a line into that dot.
            let radius = frame
                .dots
                .iter()
                .map(|(dot, radius)| (dot.distance(at), *radius))
                .filter(|(distance, _)| *distance < 0.5)
                .map(|(_, radius)| radius)
                .fold(0.0_f32, f32::max);
            if radius >= OBSTACLE_RADIUS && segment_distance(at, one, two) < radius {
                crossed += 1;
            }
        }
        worst = worst.max(crossed);
        if crossed > CROSSED_NODES {
            crowded += 1;
        }
    }
    eprintln!(
        "blob: {crowded} of the {} heaviest relations cross more than {CROSSED_NODES} dots of \
         at least {OBSTACLE_RADIUS:.0} px radius (worst {worst}); {painted} of those {} are \
         painted at all",
        heavy.len(),
        heavy.len()
    );
    assert!(
        crowded <= CROWDED_CEILING,
        "{crowded} of the {} heaviest relations are buried under more than {CROSSED_NODES} \
         dots (ceiling {CROWDED_CEILING}, worst {worst} dots)",
        heavy.len()
    );
}

// --------------------------------------------------------------------------- 4. zoom

/// The camera zooms both ways from both gestures, and the dot under the pointer stays
/// under the pointer: a zoom that walks the picture away from the cursor is a zoom the
/// reader has to chase.
#[test]
fn pinch_and_wheel_zoom_the_camera_and_hold_the_anchor() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = page_of(&archive, &path);
    let mut plate = Plate::new();
    let first = plate.settle(&mut view);

    // A dot to hold the pointer on: the strongest node, at the centre of its circle.
    let strongest = view.strongest()[0];
    let anchor = projected(first.plate, &view, strongest);

    // The wheel first: it is the gesture every mouse has, and its numbers should be in the
    // log even on a day the trackpad gesture is still missing.
    let settled = view.zoom;
    plate.wheel(anchor, 90.0, &mut view);
    let wheeled = plate.settle(&mut view);
    let wheeled_in = view.zoom;
    let drift_in = projected(wheeled.plate, &view, strongest).distance(anchor);
    plate.wheel(anchor, -90.0, &mut view);
    let back = plate.settle(&mut view);
    let wheeled_out = view.zoom;
    let drift_out = projected(back.plate, &view, strongest).distance(anchor);
    eprintln!(
        "blob: wheel {settled:.3} -> {wheeled_in:.3} -> {wheeled_out:.3}; the anchor node moved \
         {drift_in:.2} / {drift_out:.2} px"
    );
    assert!(
        wheeled_in > settled,
        "a wheel step up left the zoom at {wheeled_in:.3} (was {settled:.3})"
    );
    assert!(
        wheeled_out < wheeled_in,
        "a wheel step down went from {wheeled_in:.3} to {wheeled_out:.3}: not monotonic"
    );
    assert!(
        drift_in <= 1.0 && drift_out <= 1.0,
        "the dot under the pointer moved {drift_in:.2} / {drift_out:.2} px (allowed 1 px) \
         under the wheel: the picture is not anchored to the cursor"
    );

    // Then the pinch: the trackpad's own gesture, read by `i.zoom_delta()`.
    let before = view.zoom;
    plate.pinch(anchor, 1.35, &mut view);
    let pinched = plate.settle(&mut view);
    let pinched_in = view.zoom;
    let pinch_drift = projected(pinched.plate, &view, strongest).distance(anchor);
    plate.pinch(anchor, 0.60, &mut view);
    plate.settle(&mut view);
    let pinched_out = view.zoom;
    eprintln!("blob: pinch {before:.3} -> {pinched_in:.3} -> {pinched_out:.3}");
    assert!(
        pinched_in > before,
        "a pinch of 1.35 was delivered to the page (`i.zoom_delta()` = 1.35) and left the \
         zoom at {pinched_in:.3} (was {before:.3}): the page ignores the trackpad gesture"
    );
    assert!(
        pinched_out < pinched_in,
        "a pinch of 0.60 went from {pinched_in:.3} to {pinched_out:.3}: not monotonic"
    );
    assert!(
        pinch_drift <= 1.0,
        "the dot under the pointer moved {pinch_drift:.2} px (allowed 1 px) under the pinch"
    );
}

// --------------------------------------------------------------------------- 5. focus

/// A hub can be isolated: a double click on the busiest dot draws its neighbourhood only,
/// and Escape brings the whole graph back.
#[test]
fn double_click_isolates_a_hub_and_escape_restores_the_plate() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = page_of(&archive, &path);
    let mut plate = Plate::new();
    let frame = plate.settle(&mut view);
    let hub = view.strongest()[0];
    let neighbours = view.around(hub).len();
    let whole = frame.dots.len();
    // The plate the reader opens on is the ranked picture (at most `ACTORS_AT_FIT` entities),
    // which is the "whole graph" a double click has to undo.
    assert!(
        !frame.dots.is_empty() && whole <= 60,
        "the frame paints {whole} of {} nodes: there is no opening picture to isolate from",
        view.nodes.len()
    );
    let at = projected(frame.plate, &view, hub);
    eprintln!(
        "blob: hub {:?} has {neighbours} neighbours; the whole plate paints {whole} dots",
        view.name_of(hub)
    );

    plate.double_click(at, &mut view);
    let focused = plate.settle(&mut view);
    eprintln!(
        "blob: after the double click the plate paints {} dots (neighbourhood is {})",
        focused.dots.len(),
        neighbours + 1
    );
    assert!(
        focused.dots.len() <= neighbours + 1,
        "a double click on {:?} left {} dots on the plate; an isolated hub shows itself and \
         its {neighbours} neighbours, not the whole set of {whole}",
        view.name_of(hub),
        focused.dots.len()
    );
    assert!(
        focused.dots.len() >= 2,
        "the isolated plate paints {} dots: a hub with {neighbours} neighbours is gone too, \
         so the double click emptied the picture instead of narrowing it",
        focused.dots.len()
    );

    plate.key(egui::Key::Escape, &mut view);
    let restored = plate.settle(&mut view);
    eprintln!(
        "blob: Escape paints {} dots again (was {whole})",
        restored.dots.len()
    );
    assert!(
        restored.dots.len() >= whole,
        "Escape left {} of the {whole} dots on the plate: the isolation is not undone",
        restored.dots.len()
    );
}
