//! `geo.rs` pinned from outside its own module.
//!
//! The unit tests beside the code can reach the private fields, so they can pass by
//! agreeing with an implementation detail. These cannot: a test target is its own crate
//! and sees only what `atlas_desktop::geo` declares `pub`, so everything here is written
//! against the public surface — a camera, a place, a pixel, a ring. What is asserted is
//! the same contract `views/map.rs` is built on, on the two plate sizes the window
//! actually shows: the Overview's card, and the smallest window eframe is allowed to
//! open.
//!
//! `geo` arrives from the library, once. The path-include this file used to carry
//! compiled a second copy of the module into this binary — the same seventeen hundred
//! lines built twice per run, and only one of the two copies was code the app ships.

use atlas_desktop::geo::{self, Bounds, Camera, Geo, Point};

/// The Overview's map card, and the minimum inner size of the window
/// (`main.rs`, `with_min_inner_size([860, 560])`).
const PLATES: [(f32, f32); 2] = [(1000.0, 470.0), (860.0, 560.0)];

/// The plate is 360 by 180 degrees; these are its four corners.
const CORNERS: [(f32, f32); 4] =
    [(-180.0, 90.0), (180.0, 90.0), (-180.0, -90.0), (180.0, -90.0)];

fn close(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() < eps
}

/// The plate covers the box: no bare background beside the world, on either axis.
fn covers(camera: &Camera, size: (f32, f32)) -> bool {
    let north_west = camera.project(Geo::new(-180.0, 90.0));
    let south_east = camera.project(Geo::new(180.0, -90.0));
    north_west.x <= -size.0 / 2.0 + 1e-3
        && north_west.y <= -size.1 / 2.0 + 1e-3
        && south_east.x >= size.0 / 2.0 - 1e-3
        && south_east.y >= size.1 / 2.0 - 1e-3
}

/// The plate's own width and height in view pixels.
fn extent(camera: &Camera) -> (f32, f32) {
    let north_west = camera.project(Geo::new(-180.0, 90.0));
    let south_east = camera.project(Geo::new(180.0, -90.0));
    (south_east.x - north_west.x, south_east.y - north_west.y)
}

/// A coastline that wiggles, closed the way GeoJSON closes a ring.
fn ring(count: usize) -> Vec<Geo> {
    let mut out: Vec<Geo> = (0..count)
        .map(|index| {
            let t = index as f32 / (count - 1) as f32;
            Geo::new(-10.0 + 20.0 * t, 40.0 + 0.7 * (t * 41.0).sin() * (t * 13.0).cos())
        })
        .collect();
    let first = out[0];
    out.push(first);
    out
}

#[test]
fn the_fill_floor_leaves_no_air_on_either_axis() {
    for size in PLATES {
        let camera = Camera::new(size);
        assert!(covers(&camera, size), "{size:?}: the opening view has air beside it");
        assert!(close(camera.zoom(), 1.0, 1e-6), "{size:?} opens at ×{}", camera.zoom());
        // and it is flush, not merely generous: the tighter axis is exact to the pixel
        let (width, height) = extent(&camera);
        let flush = close(width, size.0, 1e-3) || close(height, size.1, 1e-3);
        assert!(flush, "{size:?}: plate is {width} x {height}, the box {size:?}");
        // the plate keeps the world's 2:1 shape at every size
        assert!(close(width / height, 2.0, 1e-3), "{size:?}: {} x {height}", width);
        // the plate's own corners are the box's, or beyond them
        for (lon, lat) in CORNERS {
            let at = camera.project(Geo::new(lon, lat));
            assert!(
                at.x.abs() >= size.0 / 2.0 - 1e-3 && at.y.abs() >= size.1 / 2.0 - 1e-3,
                "{size:?}: corner {lon},{lat} sits inside the box at {at:?}"
            );
        }
    }
}

#[test]
fn zooming_out_stops_at_the_whole_plate() {
    for size in PLATES {
        let mut camera = Camera::new(size);
        camera.zoom_at(Point::new(0.0, 0.0), 6.0);
        for _ in 0..40 {
            camera.zoom_at(Point::new(size.0 / 2.0, -size.1 / 2.0), 0.5);
        }
        assert!(close(camera.zoom(), 1.0, 1e-4), "{size:?} zoomed out to ×{}", camera.zoom());
        assert!(covers(&camera, size), "{size:?}: zooming out opened air");
    }
}

#[test]
fn the_zoom_ceiling_is_ninety_six_times_the_fit_and_it_holds() {
    for size in PLATES {
        let mut camera = Camera::new(size);
        for _ in 0..80 {
            camera.zoom_at(Point::new(-120.0, 60.0), 2.0);
        }
        // 96× is the contract: 267 pixels to the degree on a 1000 px card, where the
        // 1:50m geometry's own segments are already 25 px long
        assert!(close(camera.zoom(), 96.0, 1e-3), "{size:?} topped out at ×{}", camera.zoom());
        // at the ceiling, more zoom is not more map
        let before = (camera.offset(), camera.zoom());
        camera.zoom_at(Point::new(-120.0, 60.0), 2.0);
        assert!(close(camera.zoom(), before.1, 1e-6), "{size:?} moved past the ceiling");
        assert!(close(camera.offset().x, before.0.x, 1e-6) && close(camera.offset().y, before.0.y, 1e-6));
        assert!(covers(&camera, size), "{size:?}: the ceiling broke the coverage");
    }
}

#[test]
fn a_drag_stops_with_the_plate_s_edge_on_the_box_s_edge() {
    for size in PLATES {
        // at the opening zoom, nothing can move on the axis that is exactly filled
        let mut camera = Camera::new(size);
        camera.pan((1.0e6, 1.0e6));
        let corner = camera.project(Geo::new(-180.0, 90.0));
        assert!(close(corner.x, -size.0 / 2.0, 1e-3), "{size:?}: west edge at {}", corner.x);
        assert!(corner.y <= -size.1 / 2.0 + 1e-3, "{size:?}: north edge at {}", corner.y);
        // the other way, and from a zoom where both axes have slack to spend
        let mut camera = Camera::new(size);
        camera.zoom_at(Point::new(0.0, 0.0), 5.0);
        for delta in [(1.0e6, 1.0e6), (-1.0e6, 1.0e6), (1.0e6, -1.0e6), (-1.0e6, -1.0e6)] {
            camera.pan(delta);
            assert!(covers(&camera, size), "{size:?}: drag {delta:?} left air");
            // one of the four corners is now exactly on the box, and none is inside
            let flush = CORNERS.iter().any(|(lon, lat)| {
                let at = camera.project(Geo::new(*lon, *lat));
                close(at.x.abs(), size.0 / 2.0, 1e-3) || close(at.y.abs(), size.1 / 2.0, 1e-3)
            });
            assert!(flush, "{size:?}: the drag stopped short of the edge");
        }
        // and nothing outside the plate is ever under the box
        for (x, y) in [
            (-size.0 / 2.0, -size.1 / 2.0),
            (size.0 / 2.0, size.1 / 2.0),
            (size.0 / 2.0, -size.1 / 2.0),
            (-size.0 / 2.0, size.1 / 2.0),
        ] {
            let at = camera.unproject(Point::new(x, y));
            assert!(
                at.lon.abs() <= 180.0 + 1e-3 && at.lat.abs() <= 90.0 + 1e-3,
                "{size:?}: ({x}, {y}) is off the plate at {at:?}"
            );
        }
    }
}

#[test]
fn a_zoom_at_a_box_corner_keeps_the_place_under_it() {
    for size in PLATES {
        let corners = [
            Point::new(-size.0 / 2.0, -size.1 / 2.0),
            Point::new(size.0 / 2.0, -size.1 / 2.0),
            Point::new(-size.0 / 2.0, size.1 / 2.0),
            Point::new(size.0 / 2.0, size.1 / 2.0),
        ];
        for anchor in corners {
            for factor in [1.5_f32, 3.0, 0.6, 0.8] {
                let mut camera = Camera::new(size);
                // a zoom with room to move in both directions, so the pointer is what
                // the anchor is holding and not the plate's edge
                camera.zoom_at(Point::new(0.0, 0.0), 8.0);
                let before = camera.unproject(anchor);
                camera.zoom_at(anchor, factor);
                let after = camera.unproject(anchor);
                let lost = (after.lon - before.lon).abs().max((after.lat - before.lat).abs());
                assert!(
                    lost < 1e-3,
                    "{size:?}: ×{factor} at {anchor:?} slid the anchor by {lost} degrees"
                );
                assert!(covers(&camera, size), "{size:?}: the corner zoom left air");
            }
        }
        // the same at the deepest zoom, where the plate is far bigger than the box
        let mut camera = Camera::new(size);
        camera.zoom_at(Point::new(0.0, 0.0), 90.0);
        let anchor = Point::new(size.0 / 2.0, size.1 / 2.0);
        let before = camera.unproject(anchor);
        camera.zoom_at(anchor, 1.02);
        let after = camera.unproject(anchor);
        assert!(
            (after.lon - before.lon).abs() < 1e-4 && (after.lat - before.lat).abs() < 1e-4,
            "{size:?}: the anchor slid at the ceiling: {before:?} -> {after:?}"
        );
    }
}

#[test]
fn decimate_keeps_the_ring_s_ends_and_never_reorders_it() {
    let ring = ring(240);
    for size in PLATES {
        for factor in [1.0_f32, 2.0, 6.0, 96.0] {
            let mut camera = Camera::new(size);
            camera.zoom_at(Point::new(0.0, 0.0), factor);
            let screen: Vec<Point> = ring.iter().map(|at| camera.project(*at)).collect();
            let path = geo::decimate(&ring, &camera);
            // both ends survive, and they are the ring's own first and last vertex
            assert_eq!(path[0], screen[0], "{size:?} ×{factor}: the first vertex went");
            assert_eq!(
                path[path.len() - 1],
                screen[screen.len() - 1],
                "{size:?} ×{factor}: the closing vertex went"
            );
            // the ring still closes on the point it closed on
            assert_eq!(path[0], path[path.len() - 1], "{size:?} ×{factor}: it no longer closes");
            // a path is a subsequence: every point is one of the ring's, in order,
            // never twice, never invented and never moved
            let mut cursor = 0usize;
            for point in &path {
                let found = screen[cursor..]
                    .iter()
                    .position(|other| close(other.x, point.x, 1e-4) && close(other.y, point.y, 1e-4));
                let Some(offset) = found else {
                    panic!("{size:?} ×{factor}: {point:?} is not a vertex of the ring from {cursor}");
                };
                cursor += offset + 1;
            }
            // thinning is a level of detail: never fewer than the two ends, and the
            // world view is the one that has to give points back
            assert!(path.len() >= 3, "{size:?} ×{factor}: {} points", path.len());
            if factor <= 2.0 {
                assert!(
                    path.len() < ring.len(),
                    "{size:?} ×{factor}: no thinning at all ({} points)",
                    path.len()
                );
            }
        }
    }
}

#[test]
fn a_camera_eased_toward_a_target_stays_settled() {
    for size in PLATES {
        let mut from = Camera::new(size);
        from.zoom_at(Point::new(-size.0 / 3.0, size.1 / 4.0), 2.5);
        from.pan((-90.0, 40.0));
        let mut to = Camera::new(size);
        to.zoom_at(Point::new(size.0 / 3.0, -size.1 / 4.0), 14.0);
        to.pan((1.0e6, -1.0e6));
        for step in 0..=20 {
            let alpha = step as f32 / 20.0;
            let between = from.toward(to, alpha);
            assert!(covers(&between, size), "{size:?}: step {step} of the ease left air");
            // the ends are the ends
            if step == 0 {
                assert_eq!(between, from, "{size:?}: alpha 0 is not the camera it started from");
            }
            if step == 20 {
                assert_eq!(between, to, "{size:?}: alpha 1 is not the target");
            }
        }
        // an alpha outside the range is clamped, not extrapolated past the plate
        assert_eq!(from.toward(to, -1.0), from);
        assert_eq!(from.toward(to, 2.0), to);
    }
}

#[test]
fn a_ring_is_culled_by_where_it_is_and_how_big_it_is() {
    let camera = Camera::new((1000.0, 470.0));
    let continent = Bounds::of(&ring(64)).expect("a ring has a box");
    assert!(continent.shows(&camera), "a ring in the middle of the plate is drawn");
    // an islet 0.04° across is a ninth of a pixel at the fit — a step of grey, not a
    // coastline — and three and a half pixels once the plate is zoomed onto it
    let speck = Bounds::of(&[
        Geo::new(0.0, 0.0),
        Geo::new(0.04, 0.0),
        Geo::new(0.04, 0.04),
        Geo::new(0.0, 0.0),
    ])
    .expect("a speck has a box");
    assert!(!speck.shows(&camera), "0.04° at the fit is {:.2} px", 0.04 * 2.778);
    let mut deep = camera;
    deep.zoom_at(camera.project(Geo::new(0.02, 0.02)), 32.0);
    assert!(speck.shows(&deep), "0.04° at ×{} is {:.1} px", deep.zoom(), 0.04 * 2.778 * deep.zoom());
    // and a ring off the box is not drawn however large it is
    let far = Bounds::of(&[Geo::new(120.0, -10.0), Geo::new(160.0, 10.0), Geo::new(140.0, 0.0)])
        .expect("a far ring has a box");
    assert!(!far.shows(&deep), "a ring off the box is not drawn");
}
