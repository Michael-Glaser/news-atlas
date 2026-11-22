//! The coast at every zoom: every ring the plate can draw arrives as an *outline*.
//!
//! The complaint this file exists for: *while zooming, country borders disappear or look
//! broken*. It can happen in three ways, and each is one claim here.
//!
//! 1. **A ring is not stroked at all.** `views::map::World::frame` picks the rings a camera can
//!    see and simplifies them — a budget may change the *detail* and never the *contents*
//!    (`MAX_SEGMENTS`, `TIERS` in `src/views/map.rs`). So the number of stroked outlines must
//!    equal the number of rings the camera can see, at every zoom and every pan. The rings are
//!    recomputed here from the vendored file with the same parse and the same antimeridian split
//!    (`src/views/map.rs`, `World::from_geojson` and `split_at_the_antimeridian`), so the
//!    expected count is ground truth and not a second copy of the answer.
//! 2. **A ring is stroked, but not as an outline.** This is the half that was reported and the
//!    half the first version of this file missed: it asked only that a *path exist* for each
//!    ring, and the plate passed that while 287 of 841 paths at the world view were a single
//!    point — a ring that draws nothing. So the count is asked of the rings **with three or
//!    more points**, and a one-point path is its own failure at every cell.
//! 3. **A ring is clipped away by the plate.** The outline is emitted whole and the painter's
//!    scissor cuts pixels, so a ring at the plate's edge must still arrive with its geometry:
//!    each path's near edge has to reach the plate, and its far edge is allowed to run off it —
//!    which is exactly what proves the geometry was not clipped.
//!
//! A ring smaller than the hairline (a box under one pixel across at this zoom, `geo.rs:37`
//! `HAIRLINE_PX`) is a speck no stroke can draw: those are **counted and reported**, never
//! demanded. A drawing that skipped a visible island is a bug; a drawing that skipped a
//! thousandth of a pixel is a pixel.
//!
//! The stroke is a screen-space hairline: `Stroke::new(theme::HAIRLINE_WIDTH, theme::DIM)` in
//! `MapView`'s paint pass, and `theme::HAIRLINE_WIDTH` is 1.0 px (`src/theme.rs`, "the hairline").
//! It is asserted to be at least a pixel and to be independent of the zoom, because a width in
//! world units would shrink the coast to nothing as the reader zooms in — the same complaint from
//! the other end.
//!
//! The three claims above are read off `World::frame` — the geometry a frame *builds*. The last
//! test asks the same questions of the picture: it drives the shipped `MapView` in a headless
//! `egui::Context` and reads the `Shape::Path`s it actually painted, at 1×, 4×, 16×, 32×, 64× and
//! 96×, zoomed the way a reader zooms (wheel events over a pointer parked on land) and measured
//! as one **physical** pixel of stroke, because a hairline is drawn in points.
//!
//! That half is also where the fill's one self-inflicted fault was found and fixed. The plate
//! fills by the even-odd rule, so a ring that crosses *itself* paints the little loop it cut off
//! as a hole in the land. Measured on the shipped file before the fix: at the world view 8 rings
//! cut themselves, the widest loop 0.463 px², one of them coming out as page grey inside a
//! country; at ×4.2, 2 rings, the widest loop 1.055 px², two holes. They came from `apart`'s
//! thinning, which could drop the vertex of a needle shorter than the hairline and leave two
//! nearly coincident legs drawn across each other; `apart` now removes the needle itself, and
//! the same measurement is 0 loops at the world view (and 19 % fewer segments: 14 546 → 11 728)
//! and one sub-pixel loop of 0.125 px² at ×4.2, which is an eighth of a pixel under the hairline
//! drawn over it. The assertion is written at that bar — no loop of a square pixel or more — and
//! the sub-pixel ones are printed, like the specks.

use std::time::Instant;

use egui::{pos2, Rect, Vec2};

use atlas_desktop::geo::{Bounds, Camera, Geo, Point};
use atlas_desktop::theme;
use atlas_desktop::views::map::{MapView, World, GRATICULE_STEP, MAX_SEGMENTS, TIERS};

/// The card the map is drawn in: 1 000 × 470, the plate the Overview allocates.
const CARD: (f32, f32) = (1000.0, 470.0);

/// The zoom grid: the floor (1×, the whole plate), the ceiling (96×, `geo::MAX_ZOOM`, private,
/// so it is *reached* by zooming and then read back through `Camera::zoom`), and eleven steps
/// between them, geometric so every octave is sampled twice.
const ZOOMS: [f32; 13] = [1.0, 1.2, 1.5, 2.0, 2.5, 3.2, 4.2, 5.5, 7.5, 11.0, 18.0, 42.0, 96.0];

/// The pan positions, as pixels asked of the camera (it clamps to the plate's own slack): the
/// middle, the four corners of the world, and two mid-edges.
const PANS: [(f32, f32); 7] = [
    (0.0, 0.0),
    (1.0e6, 0.0),
    (-1.0e6, 0.0),
    (0.0, 1.0e6),
    (0.0, -1.0e6),
    (1.0e6, 1.0e6),
    (-1.0e6, -1.0e6),
];

/// Frames per measurement, keeping the fastest: a frame's cost is a lower bound on a machine
/// that is compiling four other crates, so the minimum is the only honest reading.
const TURNS: usize = 5;

/// The fewest points an outline can have. Two points are a line across whatever the ring
/// encloses, one point draws nothing at all.
const OUTLINE: usize = 3;

/// How far outside the plate a stroked ring may be drawn and still be a ring the cull was
/// right to keep: the cull tests the ring's box with one hairline of slack (`geo.rs:269`), and
/// a simplifier may drop the vertex that reached in. Eight pixels is that slack with room for
/// a simplified outline; a ring the camera cannot show is a plate away, not eight pixels.
const EDGE_PX: f32 = 8.0;

fn plate() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(CARD.0, CARD.1))
}

fn centre() -> egui::Pos2 {
    plate().center()
}

/// The vendored outlines, as the window reads them.
fn outlines() -> String {
    let path = atlas_desktop::assets::geojson();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display()))
}

/// The rings the file holds, recomputed here: the same parse and the same antimeridian split
/// as `World::from_geojson` / `push_polygon` / `split_at_the_antimeridian`
/// (`src/views/map.rs:351-401`).
///
/// Kept deliberately literal: this is the *expectation*, and an expectation built out of the
/// implementation's own helpers could only ever agree with it.
fn rings(text: &str) -> Vec<Vec<Geo>> {
    let root: serde_json::Value = serde_json::from_str(text).expect("the vendored file is JSON");
    let features = root
        .get("features")
        .and_then(|value| value.as_array())
        .expect("the vendored file is a FeatureCollection");
    let mut out: Vec<Vec<Geo>> = Vec::new();
    for feature in features {
        let Some(geometry) = feature.get("geometry") else { continue };
        let kind = geometry.get("type").and_then(|value| value.as_str()).unwrap_or("");
        let Some(coordinates) = geometry.get("coordinates") else { continue };
        let polygons: Vec<&serde_json::Value> = match kind {
            "Polygon" => vec![coordinates],
            "MultiPolygon" => {
                coordinates.as_array().map(|items| items.iter().collect()).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        for polygon in polygons {
            let Some(ring_list) = polygon.as_array() else { continue };
            for ring in ring_list {
                let Some(points) = ring.as_array() else { continue };
                let mut line: Vec<Geo> = Vec::new();
                for point in points {
                    let Some(pair) = point.as_array() else { continue };
                    if pair.len() < 2 {
                        continue;
                    }
                    let (Some(lon), Some(lat)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                        continue;
                    };
                    if !lon.is_finite() || !lat.is_finite() || lon.abs() > 180.0 || lat.abs() > 90.0 {
                        continue;
                    }
                    line.push(Geo::new(lon as f32, lat as f32));
                }
                // the antimeridian split: a jump wider than half the world is not coastline
                let mut piece: Vec<Geo> = Vec::new();
                for at in &line {
                    if let Some(before) = piece.last() {
                        if (at.lon - before.lon).abs() > 180.0 {
                            if piece.len() >= 2 {
                                out.push(std::mem::take(&mut piece));
                            } else {
                                piece.clear();
                            }
                        }
                    }
                    piece.push(*at);
                }
                if piece.len() >= 2 {
                    out.push(piece);
                }
            }
        }
    }
    out
}

/// Every ring of the file with its box: what the frame is allowed to skip and nothing else.
fn boxes(text: &str) -> Vec<Bounds> {
    rings(text).iter().filter_map(|ring| Bounds::of(ring)).collect()
}

/// The camera at `factor` × the fit, panned by `pan` (the camera clamps the pan itself, so
/// 1e6 means "as far as this zoom allows").
fn camera(factor: f32, pan: (f32, f32)) -> Camera {
    let mut camera = Camera::new(CARD);
    camera.zoom_at(Point::new(0.0, 0.0), factor);
    camera.pan(pan);
    camera
}

/// Where a ring's box lands on the plate, in pixels: computed from the projection, so it is an
/// expectation and not a copy of `geo::Bounds::shows`.
fn projected(bounds: &Bounds, camera: &Camera, centre: egui::Pos2) -> Rect {
    let corner = |lon: f32, lat: f32| {
        let point = camera.project(Geo::new(lon, lat));
        egui::pos2(centre.x + point.x, centre.y + point.y)
    };
    let mut rect = Rect::NOTHING;
    for lon in [bounds.west, bounds.east] {
        for lat in [bounds.south, bounds.north] {
            let at = corner(lon, lat);
            rect = rect.union(Rect::from_min_max(at, at));
        }
    }
    rect
}

/// The rings this camera can see *as something a hairline can draw*, and the ones it cannot.
///
/// A ring whose box is under one pixel across is a speck at this zoom (`geo.rs`'s
/// `HAIRLINE_PX`), and skipping it is the level of detail, not a lost border. Everything else
/// on the plate has to arrive as an outline.
fn drawable(boxes: &[Bounds], camera: &Camera, centre: egui::Pos2) -> (usize, usize) {
    drawable_in(boxes, camera, centre, plate())
}

/// The same count against a plate of any shape: the view letterboxes the world into a 2:1 box
/// inside the card, so the box a ring must reach is the plate the frame painted.
fn drawable_in(
    boxes: &[Bounds],
    camera: &Camera,
    centre: egui::Pos2,
    plate: Rect,
) -> (usize, usize) {
    let plate = plate.expand(1.0);
    let mut drawable = 0;
    let mut specks = 0;
    for bounds in boxes {
        let at = projected(bounds, camera, centre);
        if !at.intersects(plate) {
            continue;
        }
        if at.width().max(at.height()) >= 1.0 {
            drawable += 1;
        } else {
            specks += 1;
        }
    }
    (drawable, specks)
}

/// Where a path lands, and whether every vertex of it is a number.
fn span(path: &[egui::Pos2], where_: &str, index: usize) -> Rect {
    let mut bounds = Rect::NOTHING;
    for point in path {
        assert!(
            point.x.is_finite() && point.y.is_finite(),
            "at {where_}, path {index} has a vertex at {point:?}"
        );
        bounds = bounds.union(Rect::from_min_max(*point, *point));
    }
    bounds
}

/// The three ways the coast can fail, checked at every cell of the zoom × pan grid.
///
/// One test rather than three, because the three claims are one picture: a frame either has the
/// ring, has it as an outline, or has it in the plate. The message names the cell, so a failure
/// says *which* zoom and which pan.
#[test]
fn every_drawable_ring_arrives_as_an_outline_at_every_zoom_and_pan() {
    let text = outlines();
    let world = World::from_geojson(&text);
    let boxes = boxes(&text);
    assert!(
        boxes.len() > 500 && world.lines() == boxes.len(),
        "the file gave {} rings and the world parsed {}: the expectation and the \
         implementation already disagree about the file",
        boxes.len(),
        world.lines()
    );

    let mut cells = 0usize;
    let mut specks = 0usize;
    let mut worst = (0usize, 0usize, 0.0_f32, (0.0_f32, 0.0_f32));
    for factor in ZOOMS.iter() {
        for pan in PANS.iter() {
            let camera = camera(*factor, *pan);
            let frame = world.frame(&camera, centre());
            let where_ = format!("zoom ×{factor} (reached ×{:.2}), pan {pan:?}", camera.zoom());
            cells += 1;

            let (expected, tiny) = drawable(&boxes, &camera, centre());
            specks += tiny;

            // 1. every ring the plate can draw arrives as an outline, and nothing else does.
            //    The count is taken over paths with three or more points, because a path that
            //    is a dot is not a ring drawn: 287 of them passed the first version of this
            //    test, which only asked that a path exist.
            let outlines = frame.paths.iter().filter(|path| path.len() >= OUTLINE).count();
            assert_eq!(
                outlines, expected,
                "at {where_}: {expected} rings are on the plate as something a hairline can \
                 draw and {outlines} arrived as an outline ({} paths in all)",
                frame.paths.len()
            );

            // 2. no ring is a single point at any zoom: one point draws nothing, so the
            //    island is simply gone
            let dots = frame.paths.iter().filter(|path| path.len() < OUTLINE).count();
            assert_eq!(
                dots, 0,
                "at {where_}: {dots} of {} stroked rings are a dot or a line instead of an \
                 outline (the shortest has {} point(s))",
                frame.paths.len(),
                frame.paths.iter().map(|path| path.len()).min().unwrap_or(0)
            );

            // 3. finite vertices, and an outline that arrives unclipped: the near edge reaches
            //    the plate (within the hairline the visibility test allows), the far edge may
            //    run off it — which is the proof the painter, not the geometry, does the
            //    clipping
            let mut sum = 0usize;
            for (index, path) in frame.paths.iter().enumerate() {
                let bounds = span(path, &where_, index);
                // A margin of [`EDGE_PX`], not one pixel: the cull is by the ring's *box*
                // (`geo.rs:269`, one `HAIRLINE_PX` of slack), and a simplification may drop
                // the vertex that reached into the plate, so a ring whose box reaches the plate
                // can be drawn a few pixels outside it — the painter clips that, which is the
                // point of the claim. A ring emitted half a plate away still fails here.
                assert!(
                    bounds.left() <= plate().right() + EDGE_PX
                        && bounds.right() >= plate().left() - EDGE_PX
                        && bounds.top() <= plate().bottom() + EDGE_PX
                        && bounds.bottom() >= plate().top() - EDGE_PX,
                    "at {where_}, path {index} spans {bounds:?}: entirely off the plate, so a \
                     ring was emitted that the camera cannot show (or clipped away by it)"
                );
                sum += path.len();
            }
            assert_eq!(
                frame.segments,
                sum - frame.paths.len(),
                "at {where_}: {} segments reported for {} vertices in {} paths — the count the \
                 caption reads is not the geometry that is stroked",
                frame.segments,
                sum,
                frame.paths.len()
            );
            assert!(
                frame.segments <= MAX_SEGMENTS,
                "at {where_}: {} segments in one frame, over the {MAX_SEGMENTS} the plate may \
                 spend",
                frame.segments
            );
            assert!(
                frame.filled <= frame.paths.len(),
                "at {where_}: {} rings filled of {} stroked",
                frame.filled,
                frame.paths.len()
            );
            for index in 0..frame.filled {
                assert!(
                    frame.paths[index].len() >= OUTLINE,
                    "at {where_}: filled ring {index} has {} points",
                    frame.paths[index].len()
                );
            }
            assert!(frame.tier < TIERS.len(), "at {where_}: tier {}", frame.tier);

            if frame.segments > worst.0 {
                worst = (frame.segments, frame.paths.len(), *factor, *pan);
            }
        }
    }

    println!(
        "coast: {cells} cells ({} zooms × {} pans), {specks} sub-pixel specks skipped (not \
         demanded), worst frame {} segments over {} rings at zoom ×{}, pan {:?}; the file holds \
         {} rings and {} points, every one of them drawn where the plate can show it",
        ZOOMS.len(),
        PANS.len(),
        worst.0,
        worst.1,
        worst.2,
        worst.3,
        boxes.len(),
        world.points()
    );
}

/// A plate that is exactly 2:1 covers the whole world at the fit zoom, so the ring count is
/// ground truth: every ring at least a pixel across has to arrive as an outline — the one cell
/// where "nothing dropped" can be counted without trusting `Bounds::shows` at all. The rings
/// under a pixel are counted and reported, because at the world view a speck of coast is a
/// speck.
#[test]
fn a_world_view_of_a_two_to_one_plate_draws_every_ring_the_hairline_can_show() {
    let text = outlines();
    let world = World::from_geojson(&text);
    let boxes = boxes(&text);
    let box_ = (1000.0_f32, 500.0_f32);
    let mut camera = Camera::new(box_);
    camera.zoom_at(Point::new(0.0, 0.0), 1.0);
    let centre = pos2(box_.0 / 2.0, box_.1 / 2.0);
    let plate = world.frame(&camera, centre);

    let mut drawable = 0;
    let mut specks = 0;
    for bounds in &boxes {
        let at = projected(bounds, &camera, centre);
        if at.width().max(at.height()) >= 1.0 {
            drawable += 1;
        } else {
            specks += 1;
        }
    }
    assert_eq!(drawable + specks, world.lines(), "the file gave {} rings", world.lines());
    assert_eq!(
        plate.paths.len(),
        drawable,
        "at the fit zoom of a 2:1 plate the whole world is on the plate: {} drawable rings of \
         {} ({} smaller than a pixel) and {} were stroked",
        drawable,
        boxes.len(),
        specks,
        plate.paths.len()
    );
    let outlines = plate.paths.iter().filter(|path| path.len() >= OUTLINE).count();
    assert_eq!(
        outlines, drawable,
        "at the world view {} of {} stroked rings are an outline: {} are a dot or a line, and \
         a dot draws nothing",
        outlines,
        plate.paths.len(),
        plates_short(&plate.paths)
    );
    println!(
        "world view 1000×500: {} drawable rings of {} ({} sub-pixel specks set aside), {} \
         segments, tier {}, {} filled",
        drawable,
        boxes.len(),
        specks,
        plate.segments,
        plate.tier,
        plate.filled
    );
}

/// Paths with fewer than [`OUTLINE`] points — the ones a reader sees as nothing or as a line.
fn plates_short(paths: &[Vec<egui::Pos2>]) -> usize {
    paths.iter().filter(|path| path.len() < OUTLINE).count()
}

/// The hairline: one pixel on screen at every zoom, never a width that shrinks with the plate.
///
/// The stroke is drawn by the view (`src/views/map.rs`), so this reads it there: the coast is
/// stroked with `theme::HAIRLINE_WIDTH` and nothing multiplies that by the camera's scale. A
/// width in world units would be 1/96 of a pixel at the ceiling — the coast gone at the moment
/// the reader zooms in to look at it, which is the complaint reported from the inside.
#[test]
fn the_coast_is_a_one_pixel_hairline_at_every_zoom() {
    assert!(
        theme::HAIRLINE_WIDTH >= 1.0,
        "the hairline is {} px, thinner than a pixel",
        theme::HAIRLINE_WIDTH
    );
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/views/map.rs"))
        .expect("views/map.rs is part of the crate");
    assert!(
        source.contains("Stroke::new(theme::HAIRLINE_WIDTH, theme::DIM)"),
        "views/map.rs no longer strokes the coast with the theme's hairline: the width of the \
         one line this plate draws is not the one place it should be"
    );
    assert!(
        !source.contains("HAIRLINE_WIDTH *") && !source.contains("* theme::HAIRLINE_WIDTH"),
        "views/map.rs scales the hairline: a stroke width that depends on the zoom disappears \
         as the reader zooms in"
    );
    assert!(GRATICULE_STEP > 0.0, "the graticule step is {GRATICULE_STEP}");
}

/// One frame's cost over the grid, min of [`TURNS`], so the number is the code's and not the
/// machine's. Also prints the segment count each zoom lands on: the budget is what decides the
/// tier, and the tier is what decides whether a coast is drawn at 4 px or at half a pixel.
#[test]
fn the_frame_cost_of_the_whole_grid_is_measured() {
    let text = outlines();
    let world = World::from_geojson(&text);
    let mut worst_ms = 0.0_f64;
    let mut worst_cell = String::new();
    let mut all: Vec<f64> = Vec::new();
    let mut rows: Vec<String> = Vec::new();
    for factor in ZOOMS {
        for pan in PANS {
            let camera = camera(factor, pan);
            let mut best = f64::INFINITY;
            let mut segments = 0;
            let mut rings = 0;
            let mut tier = 0;
            for _ in 0..TURNS {
                let started = Instant::now();
                let plate = world.frame(&camera, centre());
                best = best.min(started.elapsed().as_secs_f64() * 1000.0);
                segments = plate.segments;
                rings = plate.paths.len();
                tier = plate.tier;
            }
            all.push(best);
            if best > worst_ms {
                worst_ms = best;
                worst_cell = format!(
                    "×{factor} pan {pan:?}: {rings} rings, {segments} segments, tier {tier}"
                );
            }
            if pan == (0.0, 0.0) {
                rows.push(format!(
                    "  ×{factor:<5} {rings:>4} rings {segments:>6} segments  tier {tier}  \
                     {best:.3} ms"
                ));
            }
        }
    }
    all.sort_by(|left, right| left.total_cmp(right));
    let median = all[all.len() / 2];
    println!(
        "frame cost, min of {TURNS} per cell, {} cells: median {median:.3} ms, worst \
         {worst_ms:.3} ms ({worst_cell})",
        all.len()
    );
    println!("centred, the diagonal of the grid:");
    for row in rows {
        println!("{row}");
    }
    // the median is the claim, because a machine compiling four other crates can stall one
    // measurement and not the other ninety
    assert!(
        median < 15.0,
        "the median frame of coast costs {median:.1} ms over {} cells: a frame that cannot be \
         drawn in 15 ms is a zoom that stutters",
        all.len()
    );
    assert!(
        worst_ms < 60.0,
        "one frame of coast costs {worst_ms:.1} ms in the worst cell ({worst_cell}): a stall \
         that long is a visible hitch, whatever the median says"
    );
}

// ---------------------------------------------------------------- the painted frame
//
// The three claims above are read off `World::frame` — the geometry the plate *builds*. The
// complaint is about the picture: borders that are not always completely there while the wheel
// is turning. So this half draws the shipped view in a headless `egui::Context` and reads what
// it actually painted: one `Shape::Path` per coast ring, with the width it was stroked at.

/// The zooms the complaint is about: the fit, and five multiples of it up to the ceiling.
const AUDIT_ZOOMS: [f32; 6] = [1.0, 4.0, 16.0, 32.0, 64.0, 96.0];

/// The smallest loop a crossing can cut off and still be a hole a reader could see: one square
/// pixel. Everything under it is inside the 1 px hairline drawn over the crossing itself.
const HOLE_PX: f32 = 1.0;

/// A 60 Hz frame, the clock the plate's own easing is stated at.
const DT: f64 = 1.0 / 60.0;

/// One painted frame: the coast paths with the width each was stroked at, the plate's own
/// rectangle, what one point is worth in physical pixels, and how long the frame took.
struct Painted {
    coasts: Vec<(Vec<egui::Pos2>, f32)>,
    plate: Rect,
    points_per_pixel: f32,
    ms: f64,
}

/// A headless window that draws the shipped plate, one frame at a time.
struct Window {
    ctx: egui::Context,
    t: f64,
    /// The card the view is drawn in: the box the window gives the map.
    card: Vec2,
}

impl Window {
    fn new() -> Window {
        Window::of(CARD)
    }

    fn of(card: (f32, f32)) -> Window {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        Window { ctx, t: 0.0, card: Vec2::new(card.0, card.1) }
    }

    /// One frame, timed: the plate is drawn in a panel with no frame of its own, so the plate's
    /// box is the window's box and every pixel of the coast is a pixel of the picture.
    fn frame(&mut self, view: &mut MapView, events: Vec<egui::Event>) -> Painted {
        self.t += DT;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), self.card)),
            time: Some(self.t),
            predicted_dt: DT as f32,
            events,
            ..Default::default()
        };
        let started = Instant::now();
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let _ = view.show(ui, &[], self.card.y);
                });
        });
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut coasts: Vec<(Vec<egui::Pos2>, f32)> = Vec::new();
        for clipped in &output.shapes {
            harvest_coast(&clipped.shape, &mut coasts);
        }
        let points_per_pixel = self.ctx.pixels_per_point();
        output.drop_without_applying_deltas();
        // The plate the view painted, asked of the view: the card's own fill is the page's
        // colour too, so the largest filled rectangle is no longer the plate.
        let plate = view.plate_rect().expect("the view painted no plate");
        Painted { coasts, plate, points_per_pixel, ms }
    }
}

/// The coast paths and the plate's own fill, out of one shape tree.
///
/// A coast ring is the only `Shape::Path` the plate strokes in `theme::DIM` with no fill
/// (`src/views/map.rs`: `painter.add(egui::Shape::line(path.clone(), coast))`); the graticule and
/// the two axes are `LineSegment`s and the land is a `Mesh`, so the distinction is the shape's
/// own kind and not a count.
fn harvest_coast(shape: &egui::epaint::Shape, coasts: &mut Vec<(Vec<egui::Pos2>, f32)>) {
    match shape {
        egui::epaint::Shape::Path(path)
            if path.stroke.width > 0.0
                && path.fill == egui::Color32::TRANSPARENT
                && matches!(path.stroke.color, egui::epaint::ColorMode::Solid(colour) if colour == theme::DIM) =>
        {
            coasts.push((path.points.clone(), path.stroke.width));
        }
        egui::epaint::Shape::Vec(items) => {
            for item in items {
                harvest_coast(item, coasts);
            }
        }
        _ => {}
    }
}

/// A wheel reading that asks to zoom in by a step small enough to land near a target zoom
/// without the accumulator's queue carrying the plate past it.
fn notch() -> egui::Event {
    egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 8.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::default(),
    }
}

/// Frames until the plate stops moving, then the last painted frame.
fn settle(window: &mut Window, view: &mut MapView, at: egui::Pos2) -> Painted {
    let mut painted = window.frame(view, vec![egui::Event::PointerMoved(at)]);
    let mut last = view.camera().zoom();
    let mut still = 0;
    for _ in 0..60 {
        painted = window.frame(view, vec![egui::Event::PointerMoved(at)]);
        let zoom = view.camera().zoom();
        if (zoom - last).abs() <= zoom * 0.0002 {
            still += 1;
        } else {
            still = 0;
        }
        last = zoom;
        if still >= 3 {
            break;
        }
    }
    painted
}

/// Does the plate paint what it built — at the six zooms the complaint names?
///
/// Three claims, read off the **painted shapes** and not off the geometry the plate built, so a
/// ring that is built and then dropped by the paint pass (a width test, a cull) fails here even
/// though `World::frame` still counts it:
///
/// 1. every ring the plate can draw is a `Shape::Path` in the frame, stroked in the coast's own
///    colour and width — the count is the file's own, computed from the vendored rings;
/// 2. every one of them is at least one **physical** pixel wide, at every zoom, measured as
///    `width × pixels_per_point` — a hairline in plate points is not a hairline on a screen
///    that scales the window;
/// 3. no ring cuts itself into a hole: a crossing whose loop is at least [`HOLE_PX`] would be
///    page grey inside a country once the even-odd fill paired it — those are zero at every
///    zoom; the sub-pixel loops that are left are reported, not demanded, like the specks.
///
/// The plate is zoomed the way a reader zooms it — wheel events over a pointer parked on land at
/// 10°E 50°N — because a ladder through open ocean would pass by painting nothing.
#[test]
fn the_painted_coast_is_the_whole_coast_at_every_zoom() {
    let text = outlines();
    let world = World::from_geojson(&text);
    let boxes = boxes(&text);
    let mut view = MapView::new(&atlas_desktop::assets::geojson());
    let mut window = Window::new();
    // two frames: the first lays the plate out and fits the camera to it
    window.frame(&mut view, Vec::new());
    let laid = window.frame(&mut view, Vec::new());
    let centre = laid.plate.center();
    let camera = view.camera();
    let land = camera.project(Geo::new(10.0, 50.0));
    let at = pos2(centre.x + land.x, centre.y + land.y);
    settle(&mut window, &mut view, at);

    let mut last_tier = 0usize;
    for target in AUDIT_ZOOMS {
        let mut guard = 0;
        while view.camera().zoom() < target * 0.995 && guard < 4_000 {
            window.frame(&mut view, vec![egui::Event::PointerMoved(at), notch()]);
            guard += 1;
        }
        let painted = settle(&mut window, &mut view, at);
        // the clock the number is: the fastest of five settled frames, because a machine
        // compiling the rest of the workspace can only add time
        let mut ms = f64::INFINITY;
        for _ in 0..5 {
            ms = ms.min(settle(&mut window, &mut view, at).ms);
        }
        let camera = view.camera();
        let centre = painted.plate.center();
        let where_ = format!("×{target} (reached ×{:.2})", camera.zoom());
        let (expected, specks) = drawable_in(&boxes, &camera, centre, painted.plate);
        let plate = world.frame(&camera, centre);

        // 1. every ring the plate can draw was painted, and nothing that draws nothing was
        assert_eq!(
            painted.coasts.len(),
            expected,
            "at {where_}: {expected} rings are on the plate as something a hairline can draw \
             and the frame painted {} ({} built, {specks} sub-pixel specks)",
            painted.coasts.len(),
            plate.paths.len()
        );
        assert!(
            painted.coasts.len() > 2,
            "at {where_}: only {} rings were painted, so the cell proves nothing",
            painted.coasts.len()
        );
        let short = painted
            .coasts
            .iter()
            .filter(|(path, _)| path.len() < OUTLINE)
            .count();
        assert_eq!(
            short, 0,
            "at {where_}: {short} of the {} painted rings are a dot or a line instead of an \
             outline",
            painted.coasts.len()
        );
        for (index, (path, _)) in painted.coasts.iter().enumerate() {
            assert!(
                path.iter().all(|point| point.x.is_finite() && point.y.is_finite()),
                "at {where_}: painted ring {index} has a vertex that is not a number"
            );
        }

        // 2. one physical pixel of stroke, whatever the window scales itself by
        let width = painted
            .coasts
            .iter()
            .map(|(_, width)| *width)
            .fold(f32::INFINITY, f32::min);
        assert!(
            width * painted.points_per_pixel >= 1.0,
            "at {where_}: the coast is stroked {width} pt wide, which is {:.2} physical px: a \
             border thinner than a pixel disappears",
            width * painted.points_per_pixel
        );

        // 3. a ring that cuts itself punches a hole in the even-odd fill; a loop under
        //    [`HOLE_PX`] is smaller than the hairline drawn over it
        let mut crossings = 0usize;
        let mut widest = 0.0_f32;
        let mut visible: Vec<egui::Pos2> = Vec::new();
        for (path, _) in &painted.coasts {
            let (count, lobe, mut where_) = self_crossings(path);
            crossings += count;
            widest = widest.max(lobe);
            // only a loop at least [`HOLE_PX`] across survives the 1 px hairline drawn over it
            where_.retain(|(size, _)| *size >= HOLE_PX);
            visible.extend(where_.into_iter().map(|(_, at)| at));
        }
        assert!(
            widest < HOLE_PX,
            "at {where_}: a ring cuts itself into a loop of {widest:.3} px²; the even-odd fill \
             paints a loop that size as a hole in the land"
        );
        if !visible.is_empty() {
            let land =
                atlas_desktop::views::map::fill_mesh(plate.fill(), painted.plate, theme::RAISED);
            let holes = visible.iter().filter(|point| !covered(&land, **point)).count();
            assert!(
                holes == 0,
                "at {where_}: {holes} of the {} loops a ring cut off are not covered by the \
                 fill, so the land has a hole where the ring crossed itself",
                visible.len()
            );
        }

        // the detail may only grow as the reader zooms in: a tier that went backwards would
        // redraw the same coast coarser while the plate is getting bigger
        if target > AUDIT_ZOOMS[0] {
            assert!(
                plate.tier <= last_tier,
                "at {where_}: the frame is drawn at tier {}, coarser than the tier {} of the \
                 zoom before it, so the coast thins away as the reader zooms in",
                plate.tier,
                last_tier
            );
        }
        last_tier = plate.tier;

        println!(
            "{where_:<26} painted {:>4} rings (beside {specks:>4} sub-pixel specks)  \
             {:>6} segments  tier {}  \
             stroke {width:.2} pt = {:.2} px  self-crossings {crossings} (widest loop \
             {widest:.3} px²)  {ms:.2} ms",
            painted.coasts.len(),
            plate.segments,
            plate.tier,
            width * painted.points_per_pixel
        );
    }
}

/// How many times a ring's own segments cross each other, the area of the widest loop a
/// crossing cuts off, and where those loops are — the holes an even-odd fill punches there.
fn self_crossings(path: &[egui::Pos2]) -> (usize, f32, Vec<(f32, egui::Pos2)>) {
    let mut count = 0usize;
    let mut widest = 0.0_f32;
    let mut lobes: Vec<(f32, egui::Pos2)> = Vec::new();
    let segments = path.len().saturating_sub(1);
    for i in 0..segments {
        for j in (i + 2)..segments {
            if i == 0 && j + 1 == segments {
                continue;
            }
            let Some(at) = crossing(path[i], path[i + 1], path[j], path[j + 1]) else {
                continue;
            };
            count += 1;
            // the loop between the two crossing segments, closed at the crossing point
            let mut loop_: Vec<egui::Pos2> = Vec::with_capacity(j - i + 2);
            loop_.push(at);
            loop_.extend_from_slice(&path[i + 1..=j]);
            let size = area(&loop_);
            widest = widest.max(size);
            lobes.push((size, middle(&loop_)));
        }
    }
    (count, widest, lobes)
}

/// A polygon's area-weighted middle: a point inside the loop a crossing cut off, so the fill can
/// be asked whether it covers the ground the ring encloses.
fn middle(points: &[egui::Pos2]) -> egui::Pos2 {
    let mut twice = 0.0_f32;
    let mut sum = egui::Vec2::ZERO;
    for index in 0..points.len() {
        let here = points[index];
        let next = points[(index + 1) % points.len()];
        let cross = here.x * next.y - next.x * here.y;
        twice += cross;
        sum += egui::vec2((here.x + next.x) * cross, (here.y + next.y) * cross);
    }
    if twice.abs() < 1e-9 {
        return points.first().copied().unwrap_or(egui::Pos2::ZERO);
    }
    let at = sum / (3.0 * twice);
    egui::pos2(at.x, at.y)
}

/// Does a fill mesh cover this pixel?
fn covered(mesh: &egui::Mesh, at: egui::Pos2) -> bool {
    for triangle in mesh.indices.chunks_exact(3) {
        let (a, b, c) = (
            mesh.vertices[triangle[0] as usize].pos,
            mesh.vertices[triangle[1] as usize].pos,
            mesh.vertices[triangle[2] as usize].pos,
        );
        let side = |p: egui::Pos2, q: egui::Pos2| (q.x - p.x) * (at.y - p.y) - (q.y - p.y) * (at.x - p.x);
        let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
        if (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0) {
            return true;
        }
    }
    false
}

/// Where two segments cross properly, if they do. A shared endpoint or a touch is not a crossing.
fn crossing(a: egui::Pos2, b: egui::Pos2, c: egui::Pos2, d: egui::Pos2) -> Option<egui::Pos2> {
    let side = |p: egui::Pos2, q: egui::Pos2, r: egui::Pos2| -> f32 {
        (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)
    };
    let (d1, d2) = (side(c, d, a), side(c, d, b));
    let (d3, d4) = (side(a, b, c), side(a, b, d));
    if !(d1 * d2 < 0.0 && d3 * d4 < 0.0) {
        return None;
    }
    let t = d1 / (d1 - d2);
    Some(pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t))
}

/// The unsigned area of a polygon, in square pixels.
fn area(points: &[egui::Pos2]) -> f32 {
    let mut twice = 0.0_f32;
    for index in 0..points.len() {
        let here = points[index];
        let next = points[(index + 1) % points.len()];
        twice += here.x * next.y - next.x * here.y;
    }
    (twice / 2.0).abs()
}

// ------------------------------------------------------- the whole world on the plate

/// The windows the layout is audited at, as the card the map is drawn in.
const AUDITED_CARDS: [(f32, f32); 5] = [
    (700.0, 520.0),
    (900.0, 620.0),
    (1280.0, 800.0),
    (1440.0, 900.0),
    (2400.0, 1400.0),
];

/// The plate shapes the Overview actually hands the view: its card at 2.1:1 on a page with
/// room (`1000 × 476`), the 1 188 × 309 plate a 1440 × 900 window leaves once the page has paid
/// for the rollup under it, the 664 × 172 a 700 px window leaves, and a tall page's plate.
const OVERVIEW_PLATES: [(f32, f32); 4] = [
    (1000.0, 476.0),
    (1188.0, 309.0),
    (664.0, 172.0),
    (1600.0, 760.0),
];

/// The extreme vertices of the vendored file, with the name of the extreme they hold: the
/// northmost, southmost, eastmost and westmost point of every ring, then both poles.
///
/// Recomputed here from the file rather than asked of the view: an expectation built out of the
/// code under test could only ever agree with it.
fn extremes(text: &str) -> Vec<(String, Geo)> {
    let mut out: Vec<(String, Geo)> = Vec::new();
    let (mut north, mut south, mut east, mut west) =
        (Geo::new(0.0, f32::MIN), Geo::new(0.0, f32::MAX), Geo::new(f32::MIN, 0.0), Geo::new(f32::MAX, 0.0));
    for (index, ring) in rings(text).iter().enumerate() {
        for at in ring {
            if at.lat > north.lat {
                north = *at;
            }
            if at.lat < south.lat {
                south = *at;
            }
            if at.lon > east.lon {
                east = *at;
            }
            if at.lon < west.lon {
                west = *at;
            }
        }
        // per ring as well, so a ring that is the extreme in its own right is named by index
        let (mut top, mut bottom, mut right, mut left) = (ring[0], ring[0], ring[0], ring[0]);
        for at in ring {
            if at.lat > top.lat {
                top = *at;
            }
            if at.lat < bottom.lat {
                bottom = *at;
            }
            if at.lon > right.lon {
                right = *at;
            }
            if at.lon < left.lon {
                left = *at;
            }
        }
        out.push((format!("ring {index} north {:.2}", top.lat), top));
        out.push((format!("ring {index} south {:.2}", bottom.lat), bottom));
        out.push((format!("ring {index} east {:.2}", right.lon), right));
        out.push((format!("ring {index} west {:.2}", left.lon), left));
    }
    out.push(("the file's northmost".to_string(), north));
    out.push(("the file's southmost".to_string(), south));
    out.push(("the file's eastmost".to_string(), east));
    out.push(("the file's westmost".to_string(), west));
    out.push(("the north pole".to_string(), Geo::new(0.0, 90.0)));
    out.push(("the south pole".to_string(), Geo::new(0.0, -90.0)));
    out
}

/// The whole world is on the plate at every audited size, both poles included.
///
/// The complaint: *the map is not shown completely*. The plate used to be fitted to the world's
/// **width** — 3.3 px/° in the Overview — so at 1440 × 900 latitudes past ±46.8° were cropped and
/// Hamburg (54.0 N), Vancouver (49.3 N) and every point north of them were off the map. The view
/// now hands `geo::Camera` the world's own 2:1 box inside the card ([`world_box`]), which makes
/// the fit the *contain* fit — `min(width / 360, height / 180)` of the card — and leaves the air
/// the card has beyond 2:1 as page beside the plate.
///
/// Measured here: every extreme vertex of the vendored file, projected through the view's own
/// camera into the plate the frame painted, must be inside that plate; the fit is asserted to be
/// the floor (the opening zoom is ×1.00) and to be `plate.width / 360` px/°, and the worst
/// extreme — the one closest to an edge — is printed per size.
#[test]
fn the_whole_world_is_on_the_plate_at_every_audited_size() {
    let text = outlines();
    let points = extremes(&text);
    let mut sizes: Vec<((f32, f32), &str)> = Vec::new();
    for card in AUDITED_CARDS {
        sizes.push((card, "window"));
    }
    for card in OVERVIEW_PLATES {
        sizes.push((card, "Overview plate"));
    }

    for (card, kind) in sizes {
        let mut view = MapView::new(&atlas_desktop::assets::geojson());
        let mut window = Window::of(card);
        // two frames: the first lays the card out and seats the camera in the plate
        window.frame(&mut view, Vec::new());
        window.frame(&mut view, Vec::new());
        let plate = view
            .plate_rect()
            .unwrap_or_else(|| panic!("the {card:?} {kind} painted no plate"));
        let camera = view.camera();
        let centre = plate.center();
        let scale = plate.width() / 360.0;
        assert!(
            plate.width() <= card.0 + 0.5 && plate.height() <= card.1 + 0.5,
            "the {kind} {card:?} painted a {:.1}×{:.1} plate, larger than the card",
            plate.width(),
            plate.height()
        );
        assert!(
            (plate.width() / plate.height() - 2.0).abs() < 0.01,
            "the {kind} {card:?} painted a plate of {:.1}×{:.1}: not the world's 2:1",
            plate.width(),
            plate.height()
        );
        assert!(
            (camera.zoom() - 1.0).abs() < 1e-4,
            "the {kind} {card:?} opens at ×{:.3}, not at the fit",
            camera.zoom()
        );
        assert!(
            (scale - plate.height() / 180.0).abs() < 0.01,
            "the {kind} {card:?}: {scale:.3} px/° across and {:.3} px/° down",
            plate.height() / 180.0
        );

        let mut worst = (f32::INFINITY, String::new());
        for (name, at) in &points {
            let projected = camera.project(*at);
            let on_plate = pos2(centre.x + projected.x, centre.y + projected.y);
            assert!(
                plate.expand(1.0).contains(on_plate),
                "the {kind} {card:?}: {name} at ({:.3}, {:.3}) is drawn at {on_plate:?}, \
                 outside the plate {plate:?}",
                at.lon,
                at.lat
            );
            // how far inside the plate it is: the smallest distance to an edge of the four
            let inside = (on_plate.x - plate.left())
                .min(plate.right() - on_plate.x)
                .min(on_plate.y - plate.top())
                .min(plate.bottom() - on_plate.y);
            if inside < worst.0 {
                worst = (inside, format!("{name} at ({:.3}, {:.3})", at.lon, at.lat));
            }
        }
        println!(
            "{kind:<15} {:.0}×{:.0} card → plate {:.0}×{:.0} at ({:.0}, {:.0}), {scale:.3} px/° \
             ({:.1}° across the card), worst extreme {:.1} px inside: {}",
            card.0,
            card.1,
            plate.width(),
            plate.height(),
            plate.left(),
            plate.top(),
            card.0 / scale,
            worst.0,
            worst.1
        );
    }
}
