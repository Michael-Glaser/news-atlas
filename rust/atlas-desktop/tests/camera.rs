//! The map's camera and its plate, against a fixture of a few rings.
//!
//! Two things are pinned here, both arithmetic and both reachable without a window:
//! `atlas_desktop::geo::Camera` — `screen = centre + (lon·k + x, −lat·k + y)`, the pan
//! that moves the world by the pointer's own delta, the zoom that keeps the place under
//! the pointer under it, the clamps at the whole plate and at the deepest level, the
//! first-frame fit and the ease — and `atlas_desktop::views::map::{World, Plate}`, the
//! GeoJSON parser (Polygon, MultiPolygon, the antimeridian split, coordinates that are
//! not on Earth) and the frame that draws a ring where the camera says it is.
//!
//! # What this file reads now
//!
//! The camera moved out of `views/map.rs` and into `geo.rs`, and the plate with it: the
//! view no longer carries a second geometry of its own. So the camera is reached as
//! [`atlas_desktop::geo::Camera`], its fields are private and it is *built* rather than
//! literalled — `Camera::new((w, h))` seats the plate in the box at ×1, `pan`, `zoom_at`,
//! `resize` and `toward` move it, and `project`/`unproject`/`offset`/`zoom` read it. The
//! old `MIN_K`/`MAX_K` constants are gone; the zoom is expressed as the multiple of the
//! fit, so its range is 1 … the ceiling, and the ceiling's own value is pinned from
//! outside by `tests/geo.rs`. `Plate` now carries `paths`, `filled`, `segments` and
//! `tier`, and the paths are `Pos2` in the plate's pixels rather than view-relative
//! `Point`s.
//!
//! # Where an assertion lives now
//!
//! `tests/geo.rs` owns the *pure* camera contract — the fill floor, the 96× ceiling, the
//! drag-against-the-edge, the zoom at a box corner, `decimate`, a camera eased toward a
//! target staying settled, and `Bounds::shows` — and this file does not re-argue it.
//! What is kept here is the same set of claims the pre-`geo` file made, re-pointed: the
//! documented formula, the round trip, a drag's exact delta, an anchored zoom, both
//! clamps, the first frame, the ease landing on its target, the parser and the frame.
//! Two assertions did not survive the move, and neither has an equivalent: `Camera` has
//! no `Default` (a view seats one with `Camera::new`, so "the default camera is inside
//! the range" is now "the first frame is"), and `near`/`arrived` is private to
//! `views/map.rs`, so the arrival is asserted through `Motion::step` — the ease ends on
//! its target and stops asking for frames, which is what the arrival test decided.
//!
//! The map's *drawing* (`MapView::show`) is not tested here: it needs a `Ui`, and this
//! file keeps to the arithmetic. `tests/map_zoom_pan.rs` owns the gesture path.

use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Rect};

use atlas_desktop::geo::{Camera, Geo, Point};
use atlas_desktop::views::map::{MapView, World};

/// The Overview's map card: 1 000 × 470, wider than the plate's 2:1, so at the opening
/// zoom the width is exactly filled and the height has a sliver to spare.
const CARD: (f32, f32) = (1000.0, 470.0);

/// The plate, in degrees: 360 by 180, centred on 0°,0°.
const PLATE_W: f32 = 360.0;
const PLATE_H: f32 = 180.0;

fn close(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() < eps
}

/// The box the plate is drawn in, as `World::frame` wants it.
fn card() -> Rect {
    Rect::from_min_size(pos2(100.0, 50.0), vec2(CARD.0, CARD.1))
}

/// The plate covers the box: no air beside the world, on either axis.
fn covers(camera: &Camera, size: (f32, f32)) -> bool {
    let north_west = camera.project(Geo::new(-PLATE_W / 2.0, PLATE_H / 2.0));
    let south_east = camera.project(Geo::new(PLATE_W / 2.0, -PLATE_H / 2.0));
    north_west.x <= -size.0 / 2.0 + 1e-3
        && north_west.y <= -size.1 / 2.0 + 1e-3
        && south_east.x >= size.0 / 2.0 - 1e-3
        && south_east.y >= size.1 / 2.0 - 1e-3
}

/// A camera with room to move on both axes: the fit, then ×4.
fn roaming() -> Camera {
    let mut camera = Camera::new(CARD);
    camera.zoom_at(Point::new(0.0, 0.0), 4.0);
    camera
}

// ------------------------------------------------------------------- the projection

#[test]
fn the_camera_maps_degrees_to_pixels_by_the_documented_formula() {
    // screen = centre + (lon·k + x, −lat·k + y), south-negative so north is up. The
    // centre is the box's middle, so what `project` answers is measured from it.
    let camera = Camera::new(CARD);

    let origin = camera.project(Geo::new(0.0, 0.0));
    assert_eq!((origin.x, origin.y), (0.0, 0.0), "the plate starts centred");

    let k = camera.project(Geo::new(1.0, 0.0)).x - origin.x;
    let east = camera.project(Geo::new(10.0, 0.0));
    assert!(
        close(east.x, 10.0 * k, 1e-3) && close(east.y, 0.0, 1e-3),
        "{east:?}"
    );

    let north = camera.project(Geo::new(0.0, 10.0));
    assert!(
        close(north.y, -10.0 * k, 1e-3) && close(north.x, 0.0, 1e-3),
        "{north:?}"
    );
    // A degree is the same length on both axes: the plate keeps its 2:1 shape.
    let up = origin.y - north.y;
    assert!(close(east.x - origin.x, up, 1e-3), "{k} px east against {up} px north");
}

#[test]
fn a_pan_moves_the_plate_by_exactly_its_offset_and_never_rescales_it() {
    let camera = roaming();
    let mut moved = camera;
    moved.pan((-35.0, 12.0));

    assert_eq!(moved.offset(), Point::new(-35.0, 12.0), "a drag is 1:1");
    assert!(close(moved.zoom(), camera.zoom(), 1e-6), "a pan never zooms");
    let before = camera.project(Geo::new(20.0, -10.0));
    let after = moved.project(Geo::new(20.0, -10.0));
    assert!(
        close(after.x - before.x, -35.0, 1e-3) && close(after.y - before.y, 12.0, 1e-3),
        "the degree moved by {:?}, not by the pointer",
        (after.x - before.x, after.y - before.y)
    );
    // Dragged past what the plate has spare, the drag stops with the edge on the box.
    let mut flush = camera;
    flush.pan((5000.0, 5000.0));
    let west = flush.project(Geo::new(-PLATE_W / 2.0, PLATE_H / 2.0));
    assert!(close(west.x, -CARD.0 / 2.0, 1e-3), "the west edge is at {}", west.x);
}

#[test]
fn a_screen_point_and_a_degree_are_the_same_point_the_other_way_round() {
    let camera = roaming();
    for (lon, lat) in [(0.0, 0.0), (52.5, 13.4), (-33.9, 151.2), (-179.9, -89.9)] {
        let there = camera.project(Geo::new(lon, lat));
        let back = camera.unproject(there);
        assert!(
            close(back.lon, lon, 1e-3) && close(back.lat, lat, 1e-3),
            "{lon},{lat} came back as {back:?}"
        );
    }
    // A whole grid of them, near the edges included: this is what a click is.
    let mut worst = 0.0_f32;
    let mut lon = -180.0;
    while lon <= 180.0 {
        let mut lat = -90.0;
        while lat <= 90.0 {
            let back = camera.unproject(camera.project(Geo::new(lon, lat)));
            worst = worst.max((back.lon - lon).abs()).max((back.lat - lat).abs());
            lat += 7.5;
        }
        lon += 11.25;
    }
    assert!(worst < 1e-3, "the round trip loses {worst} degrees");
}

#[test]
fn a_zoom_keeps_what_is_under_the_pointer_exactly_there() {
    for anchor in [Point::new(-320.0, 90.0), Point::new(0.0, 0.0), Point::new(410.0, -180.0)] {
        for factor in [1.6_f32, 0.5, 3.0, 0.75, 1.0] {
            // Room to move in both directions, away from either end of the clamp.
            let mut camera = Camera::new(CARD);
            camera.zoom_at(Point::new(0.0, 0.0), 8.0);
            let before = camera.unproject(anchor);
            let scale = camera.zoom();
            camera.zoom_at(anchor, factor);
            let after = camera.unproject(anchor);
            let lost = (after.lon - before.lon).abs().max((after.lat - before.lat).abs());
            assert!(
                close(camera.zoom(), scale * factor, 1e-3),
                "×{factor} moved the zoom to {} from {scale}",
                camera.zoom()
            );
            assert!(lost < 1e-3, "×{factor} at {anchor:?} moved the anchor {lost} degrees");
        }
    }
}

#[test]
fn the_zoom_stops_at_the_range_and_ignores_a_nonsense_factor() {
    let mut camera = Camera::new(CARD);
    // Out: the whole plate is the floor, and the plate still covers the box.
    for _ in 0..40 {
        camera.zoom_at(Point::new(300.0, 100.0), 0.5);
    }
    assert!(close(camera.zoom(), 1.0, 1e-4), "zoom out stops at the plate: {}", camera.zoom());
    assert!(covers(&camera, CARD));

    // In: the deepest level the outlines can answer. `geo`'s ceiling is private, and
    // `tests/geo.rs::the_zoom_ceiling_is_ninety_six_times_the_fit_and_it_holds` is where
    // its value is argued; here the claim is that there *is* one and that it holds.
    for _ in 0..80 {
        camera.zoom_at(Point::new(300.0, 100.0), 2.0);
    }
    let deepest = camera.zoom();
    assert!(close(deepest, 96.0, 1e-2), "zoom in stops at {deepest}");
    camera.zoom_at(Point::new(300.0, 100.0), 2.0);
    assert!(close(camera.zoom(), deepest, 1e-4), "the ceiling was exceeded");
    assert!(covers(&camera, CARD));

    // A factor that is not a positive number leaves the camera alone rather than
    // producing a NaN scale and a blank plate for the rest of the session.
    let mut steady = Camera::new(CARD);
    steady.zoom_at(Point::new(0.0, 0.0), 4.0);
    for factor in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let mut once = steady;
        once.zoom_at(Point::new(450.0, 260.0), factor);
        assert_eq!(once, steady, "factor {factor} moved the camera");
    }
}

#[test]
fn the_first_frame_fits_the_whole_world_in_the_plate() {
    // The plate is seated flush, not with a margin: the margin is the card's own padding,
    // so a camera that letterboxed the world here would be air inside a card that already
    // has some. That is why the old ×0.94 has no successor, and why "fitted" is now
    // `covers` — the world reaches past the box on the axis the card is not flush with,
    // which is cropping, not air.
    for size in [CARD, (400.0, 400.0), (1440.0, 900.0), (900.0, 300.0), (1.0, 1.0)] {
        let camera = Camera::new(size);
        assert!(covers(&camera, size), "{size:?} leaves air");
        assert!(close(camera.zoom(), 1.0, 1e-6), "{size:?} opens at {}", camera.zoom());
        assert_eq!(camera.offset(), Point::new(0.0, 0.0), "the world starts centred");
        // Nothing but the world is under the box: every corner of the box is on the plate
        // or inside it, and the axis the plate is flush with is flush exactly.
        for corner in [
            Point::new(-size.0 / 2.0, -size.1 / 2.0),
            Point::new(size.0 / 2.0, size.1 / 2.0),
            Point::new(size.0 / 2.0, -size.1 / 2.0),
            Point::new(-size.0 / 2.0, size.1 / 2.0),
        ] {
            let at = camera.unproject(corner);
            assert!(
                at.lon.abs() <= PLATE_W / 2.0 + 1e-3 && at.lat.abs() <= PLATE_H / 2.0 + 1e-3,
                "the box corner {corner:?} of {size:?} is off the plate: {at:?}"
            );
        }
    }
    let camera = Camera::new(CARD);
    let west = camera.project(Geo::new(-PLATE_W / 2.0, 0.0));
    assert!(close(west.x, -CARD.0 / 2.0, 1e-3), "the west edge is at {}", west.x);
    // The card is wider than the plate's 2:1, so the surplus is height: 15 px of the
    // world's poles are above and below the card, which is how the old margin went.
    let north = camera.project(Geo::new(0.0, PLATE_H / 2.0));
    assert!(close(north.y, -CARD.1 / 2.0 - 15.0, 1e-2), "the north edge is at {}", north.y);
}

#[test]
fn nothing_outside_the_plate_is_ever_under_the_box() {
    // Zoomed in and dragged as far as it goes on both axes.
    let mut camera = Camera::new(CARD);
    camera.zoom_at(Point::new(0.0, 0.0), 4.0);
    camera.pan((-1.0e6, -1.0e6));
    let south_east = camera.project(Geo::new(PLATE_W / 2.0, -PLATE_H / 2.0));
    assert!(close(south_east.x, CARD.0 / 2.0, 1e-3), "east edge at {}", south_east.x);
    assert!(close(south_east.y, CARD.1 / 2.0, 1e-3), "south edge at {}", south_east.y);
    for point in [
        Point::new(-CARD.0 / 2.0, -CARD.1 / 2.0),
        Point::new(CARD.0 / 2.0, CARD.1 / 2.0),
        Point::new(CARD.0 / 2.0, -CARD.1 / 2.0),
        Point::new(-CARD.0 / 2.0, CARD.1 / 2.0),
    ] {
        let at = camera.unproject(point);
        assert!(
            at.lon.abs() <= PLATE_W / 2.0 + 1e-3 && at.lat.abs() <= PLATE_H / 2.0 + 1e-3,
            "{point:?} is off the plate: {at:?}"
        );
    }
}

#[test]
fn a_gesture_can_never_leave_air_beside_the_world() {
    // The frame loop: drag, zoom about the pointer, occasionally resize. Four hundred
    // frames of the worst case, with the invariant checked every one.
    let mut size = CARD;
    let mut camera = Camera::new(size);
    for step in 0..400 {
        let sign = if step % 2 == 0 { 1.0 } else { -1.0 };
        camera.pan((sign * 373.0, -sign * 191.0));
        let anchor = Point::new(step as f32 - 200.0, 60.0 - step as f32 * 0.5);
        camera.zoom_at(anchor, 1.0 + (step % 7) as f32 * 0.31);
        if step % 11 == 0 {
            size = (700.0 + step as f32, 300.0 + step as f32 * 0.7);
            camera.resize(size);
        }
        assert!(covers(&camera, size), "frame {step} leaves air: {}", camera.zoom());
        assert!(camera.zoom() >= 1.0 && camera.zoom() <= 96.0 + 1e-3, "frame {step}");
    }
}

// ------------------------------------------------------------------------ the ease

#[test]
fn the_ease_lands_on_its_target_and_stops_asking_for_frames() {
    let mut from = Camera::new(CARD);
    from.zoom_at(Point::new(-480.0, -220.0), 3.0);
    from.pan((900.0, 400.0));
    let mut to = Camera::new(CARD);
    to.zoom_at(Point::new(470.0, 210.0), 12.0);
    to.pan((-1.0e5, 1.0e5));

    // No time, no movement; all the time, the target; more than all of it is still 1.
    assert_eq!(from.toward(to, 0.0), from);
    assert_eq!(from.toward(to, 1.0), to);
    assert_eq!(from.toward(to, 5.0), to);
    // And every step between two settled cameras is settled, so a gesture never flashes
    // air on its way.
    for step in 0..=20 {
        let between = from.toward(to, step as f32 / 20.0);
        assert!(covers(&between, CARD), "step {step} of the ease leaves air");
    }

    // The ease the view actually runs: `Motion` steps toward its target with the frame's
    // own dt, and the arrival test (`arrived`, private) is what ends it.
    let mut motion = atlas_desktop::views::map::Motion::new(to);
    assert!(!motion.is_moving(), "a motion with nothing queued is still");
    motion.zoom_by(0.4, Point::new(10.0, -10.0));
    assert!(motion.is_moving(), "a queued zoom is motion");
    let mut current = from;
    let mut frames = 0;
    loop {
        let (next, moving) = motion.step(current, 0.05);
        current = next;
        frames += 1;
        assert!(covers(&current, CARD), "frame {frames} of the ease leaves air");
        if !moving {
            break;
        }
        assert!(frames < 400, "the ease never arrived");
    }
    assert_eq!(current, motion.target, "the last frame is not the target");
    assert!(frames < 400);
}

// --------------------------------------------------------------- the GeoJSON parser

/// One `Polygon` feature with one ring of the given coordinates.
fn polygon(coordinates: &str) -> String {
    format!(
        r#"{{"type":"Feature","geometry":{{"type":"Polygon","coordinates":[{coordinates}]}}}}"#
    )
}

fn collection(features: &[String]) -> String {
    format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","))
}

#[test]
fn a_polygon_and_a_multipolygon_become_the_coastlines_they_name() {
    let square = "[[-10,-10],[10,-10],[10,10],[-10,10],[-10,-10]]";
    let (lines, points) = world_of(&collection(&[polygon(square)]));
    assert_eq!((lines, points), (1, 5), "one ring, five coordinates, none dropped");

    // A MultiPolygon is several rings in one feature: two coastlines, not one.
    let multi = format!(
        r#"{{"type":"Feature","geometry":{{"type":"MultiPolygon","coordinates":[[{square}],[{square2}]]}}}}"#,
        square2 = "[[20,20],[30,20],[30,30],[20,30],[20,20]]"
    );
    let (lines, points) = world_of(&collection(&[multi]));
    assert_eq!((lines, points), (2, 10));

    // Two features are two rings as well, whatever order the world keeps them in (it
    // holds them biggest first, so a budget is spent on the continents).
    let (lines, points) = world_of(&collection(&[polygon(square), polygon(square)]));
    assert_eq!((lines, points), (2, 10));
}

#[test]
fn a_ring_that_cannot_draw_a_line_is_not_a_coastline() {
    // One coordinate is not a line. A *two*-point ring is kept: a degenerate ring still
    // draws a segment, and the plate is the thing that decides whether it is a pixel.
    let one_point = "[[5,5]]";
    assert_eq!(world_of(&collection(&[polygon(one_point)])), (0, 0), "one point is not a line");

    let two_points = "[[5,5],[6,6]]";
    assert_eq!(
        world_of(&collection(&[polygon(two_points)])),
        (1, 2),
        "two points is one drawable segment, and the parser keeps it"
    );

    // A ring of pairs that are not a coordinate is not a ring either.
    let junk = r#"[["a","b"],[5],[7,8,9]]"#;
    assert_eq!(world_of(&collection(&[polygon(junk)])), (0, 0), "junk pairs are skipped");
}

#[test]
fn a_coordinate_that_is_not_on_earth_is_skipped_not_drawn() {
    // The parser drops the point, keeps the ring it belongs to: one bad vertex must not
    // cost the whole coastline, and it must never land on the plate at a place that does
    // not exist.
    let (lines, points) = world_of(&collection(&[polygon(
        "[[-10,-10],[200,0],[10,-10],[10,10],[-10,10]]",
    )]));
    assert_eq!((lines, points), (1, 4), "lon 200 is skipped, the other four stay");

    let (lines, points) = world_of(&collection(&[polygon("[[-10,-10],[0,91],[10,-10],[10,10]]")]));
    assert_eq!((lines, points), (1, 3), "lat 91 is skipped");

    // Every coordinate out of range leaves nothing to draw.
    assert_eq!(
        world_of(&collection(&[polygon("[[200,0],[-200,0],[0,95],[0,-95]]")])),
        (0, 0)
    );
}

#[test]
fn a_ring_that_steps_over_the_antimeridian_is_split_there() {
    // Fiji, roughly: a ring whose longitude jumps from 178° east to 178° west. Drawn as
    // one path it would be a line across the whole plate, so the parser breaks it where
    // the jump is — once, at load, not on every frame.
    let fiji = "[[177,-17],[178,-18],[-178,-18],[-178,-17],[177,-17]]";
    let world = World::from_geojson(&collection(&[polygon(fiji)]));
    assert_eq!(world.lines(), 2, "the jump is two coastlines");
    assert_eq!(world.points(), 4, "the split keeps every coordinate it can draw");

    // And nothing the plate is asked to stroke is wider than half the world: a segment
    // that crossed the antimeridian would be 356° long.
    let camera = Camera::new(CARD);
    let k = camera.project(Geo::new(1.0, 0.0)).x;
    let plate = world.frame(&camera, card().center());
    assert_eq!(plate.paths.len(), 2, "both pieces are on the plate");
    for path in &plate.paths {
        for pair in path.windows(2) {
            assert!(
                (pair[1].x - pair[0].x).abs() <= PLATE_W / 2.0 * k,
                "a segment spans {} px, which is off the plate",
                (pair[1].x - pair[0].x).abs()
            );
        }
    }
}

#[test]
fn text_that_is_not_geojson_is_an_empty_world_never_a_panic() {
    for text in [
        "",
        "not json",
        "[]",
        r#"{"type":"FeatureCollection"}"#,
        r#"{"features":"not an array"}"#,
        r#"{"features":[{"type":"Feature"}]}"#,
        r#"{"features":[{"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
        r#"{"features":[{"geometry":{"type":"Polygon"}}]}"#,
    ] {
        assert_eq!(world_of(text), (0, 0), "input {text:?} produced a coastline");
    }
    // … and a missing file is the same empty world, which is what the map draws on a
    // machine that has not vendored the outlines.
    let missing = missing_file();
    let world = World::load(&missing);
    assert_eq!((world.lines(), world.points()), (0, 0));
    assert_eq!(world.parse_ms, 0.0_f64, "a missing file took no time to parse");
    // An empty world still frames: the plate is empty, not absent.
    let plate = world.frame(&Camera::new(CARD), card().center());
    assert!(plate.paths.is_empty() && plate.segments == 0);
}

// ------------------------------------------------------------------- the plate

#[test]
fn a_frame_draws_the_ring_where_the_camera_says_and_nothing_off_the_plate() {
    let square = "[[-10,-10],[10,-10],[10,10],[-10,10],[-10,-10]]";
    let world = World::from_geojson(&collection(&[polygon(square)]));

    // The ring is ten degrees either way, so at 20 px/degree it fills 400 px of the
    // 1 000 × 470 plate: inside it. The card is flush with the world's 360° of longitude,
    // so 20 px/degree is the opening fit (2.78) zoomed ×7.2.
    let rect = card();
    let centre = rect.center();
    let mut camera = Camera::new(CARD);
    camera.zoom_at(Point::new(0.0, 0.0), 7.2);
    let k = camera.project(Geo::new(1.0, 0.0)).x;
    assert!(close(k, 20.0, 1e-3), "the camera is at {k} px/degree");

    let framed = world.frame(&camera, centre);
    assert_eq!(framed.paths.len(), 1, "one coastline on the plate");
    let line = &framed.paths[0];
    assert_eq!(line.len(), 5, "every coordinate is kept at this zoom");
    // The first vertex is the ring's first coordinate, drawn where the camera puts it —
    // the plate has no second geometry of its own.
    let there = camera.project(Geo::new(-10.0, -10.0));
    let expected = pos2(centre.x + there.x, centre.y + there.y);
    assert!(
        (line[0].x - expected.x).abs() < 1e-3 && (line[0].y - expected.y).abs() < 1e-3,
        "vertex {:?} vs camera {:?}",
        line[0],
        expected
    );
    // Every vertex is inside the box, and five points make four segments.
    for at in line {
        assert!(rect.contains(*at), "a vertex landed at {at:?}, outside {rect:?}");
    }
    assert_eq!(framed.segments, 4, "five points make four segments");
    // The fill covers a prefix of the paths — the biggest rings first — and says how many.
    assert_eq!(framed.fill().len(), framed.filled);
    assert_eq!(framed.filled, 1, "one ring, wide enough to fill");
    assert_eq!(framed.tier, 0, "one ring is not over any budget");

    // Panned a thousand pixels down, the same ring is culled: a coast nobody can see
    // costs no segment.
    let mut away = camera;
    away.pan((0.0, 1500.0));
    let far = world.frame(&away, centre);
    assert!(far.paths.is_empty() && far.segments == 0, "a ring off the plate is kept");
}

#[test]
fn a_map_view_reads_the_outlines_it_is_given_and_draws_nothing_before_its_first_frame() {
    let text = collection(&[
        polygon("[[-10,-10],[10,-10],[10,10],[-10,10],[-10,-10]]"),
        polygon("[[20,20],[30,20],[30,30],[20,30],[20,20]]"),
    ]);
    let file = TempFile::holding("mapview", &text);
    let view = MapView::new(file.path());

    let outlines = view.outlines();
    assert!(outlines.contains("2 coastlines"), "{outlines}");
    assert!(outlines.contains("10 points"), "{outlines}");
    assert!(outlines.contains("outlines read in"), "the cost is stated: {outlines}");
    assert_eq!(view.segments(), 0, "no frame has been drawn yet");

    // A view on a file that is not there is an empty plate, not a failure.
    let view = MapView::new(&missing_file());
    assert!(view.outlines().contains("0 coastlines"), "{}", view.outlines());
    assert_eq!(view.segments(), 0);
}

#[test]
fn a_plate_strokes_every_ring_it_can_see_and_fills_a_prefix_of_them() {
    // The budget decides how much of the plate is *filled*, never which coastlines exist:
    // a ring the fill could not afford would still be stroked, and a ring is never
    // dropped for being unaffordable. Twelve rings, all of them on the plate.
    //
    // Each is 100° wide and at its own latitude: a ring spanning more than half the world
    // would be split at the antimeridian and counted twice, which is a different test.
    let mut features = Vec::new();
    for index in 0..12 {
        let north = -80.0 + index as f32 * 13.0;
        features.push(polygon(&format!(
            "[[{west},{north}],[{east},{north}],[{east},{south}],[{west},{south}],[{west},{north}]]",
            west = -150.0,
            east = -50.0,
            south = north + 10.0
        )));
    }
    let world = World::from_geojson(&collection(&features));
    assert_eq!(world.lines(), 12);
    let camera = Camera::new(CARD);
    let plate = world.frame(&camera, card().center());
    let stroked: usize = plate.paths.len();
    let filled = plate.filled;
    assert_eq!(stroked, 12, "every ring the camera can see is stroked");
    assert!(filled <= stroked, "the fill is a prefix, not a second list");
    assert_eq!(plate.fill().len(), filled);
}

// ---------------------------------------------------------------------- fixtures

/// A temporary file holding GeoJSON, removed when the test that asked for it ends.
/// Never in `data/`.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn holding(name: &str, text: &str) -> TempFile {
        let path = std::env::temp_dir().join(format!(
            "atlas-desktop-camera-{}-{}.geojson",
            name,
            std::process::id()
        ));
        std::fs::write(&path, text).expect("the temp directory is writable");
        TempFile { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A path that does not exist: what the map is handed on a machine with no outlines.
fn missing_file() -> PathBuf {
    let path: PathBuf = std::env::temp_dir().join("atlas-desktop-camera-no-such-file.geojson");
    let _ = std::fs::remove_file(&path);
    path
}

/// The GeoJSON parser, seen through the two numbers it publishes: how many coastlines it
/// kept and how many coordinates they hold.
fn world_of(text: &str) -> (usize, usize) {
    let world = World::from_geojson(text);
    (world.lines(), world.points())
}
