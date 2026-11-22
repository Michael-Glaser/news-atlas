//! The projection the map draws with: degrees in, plate pixels out, and back.
//!
//! `views/map.rs` does not carry a camera of its own — every coordinate on the plate goes
//! through `geo::Camera::project`, and every click comes back through `unproject`
//! (`views/map.rs:773`, `on_plate`; `views/map.rs:858`, the centre it adds). That makes the
//! projection the one piece of the map a test can hold still, and it is worth holding: a
//! sign, a scale or an axis flipped here moves every coastline, every graticule line and
//! every pin at once, and no assertion inside a view can tell you which of the three
//! happened.
//!
//! Four properties, and the numbers measured for each:
//!
//! 1. **The round trip closes.** `unproject(project(g))` is `g` again over a grid of the
//!    whole world — 5° of longitude and latitude, both antimeridian edges included, both
//!    poles included — at four cameras: the card the view opens with, a tall card, the
//!    deepest zoom, and a card that was resized and panned. Measured: the worst error of
//!    the four is printed by the test and asserted under [`EPSILON_DEG`]; it is a few times
//!    f32's own last digit, not a tolerance the projection was tuned to.
//! 2. **The edges are the plate, not a wrap.** Equirectangular does not wrap: −180 and +180
//!    are one plate-width apart and each returns itself. Wrapping is the *view's* problem
//!    and it solves it differently, by splitting a ring where it steps over the line
//!    (`views/map.rs:376`, `split_at_the_antimeridian`), which is why the projection must
//!    not quietly wrap.
//! 3. **Garbage in never becomes NaN.** NaN, infinity and absurd numbers through every
//!    mutator (`new`, `resize`, `pan`, `zoom_at`) leave a finite camera whose plate still
//!    covers its box; and no coordinate a parser would keep (|lon| ≤ 180, |lat| ≤ 90,
//!    `views/map.rs:364`) projects to an infinity, at any zoom the camera can hold.
//! 4. **A known place lands on a known pixel.** Six cities, hand-computed for the card the
//!    view opens with, pinned to half a pixel — so a rate, a sign or an origin that moved
//!    is caught by its effect on the drawing rather than by its shape in the source.
//! 5. **The opening zoom is filled, not contained** — measured, not argued:
//!    [`fill_or_contain_at_the_opening_zoom`] runs the shipped archive's 259 stories through
//!    the view's own `cluster` at five window sizes and under both scales, and prints what
//!    each one puts off the plate and what each one leaves empty. The answer it settles —
//!    fill: at most 5% of the stories cropped and one pan away; contain: 11–33% of the
//!    window blank and the plate at 67–86% of the scale — is the reason `geo::Camera` has
//!    no letterbox setting.
//!
//! Read-only and windowless: no GPU surface, no socket, no clock. Most of it needs no
//! archive at all; the fifth property reads `data/atlas.db` read-only and says so and passes
//! when a checkout does not have one (`tests/place_stories.rs:51` does the same).

use egui::{Pos2, Rect, Vec2};

use atlas_desktop::assets;
use atlas_desktop::data::{Archive, Pin};
use atlas_desktop::geo::{Camera, Geo, Point};
use atlas_desktop::views::map::{cluster, World, CLUSTER_PX};

/// The plate, in degrees: the width of the world and the height of it (`geo.rs:24`, private
/// there). Restated here because it is a promise the drawing keeps, not an implementation
/// detail: a degree of longitude and a degree of latitude are the same number of pixels, so
/// the plate is 2:1 and `±180`/`±90` are its edges.
const PLATE_W: f32 = 360.0;
const PLATE_H: f32 = 180.0;

/// The deepest zoom, as a multiple of the zoom that fills the box (`geo.rs:31`,
/// `MAX_ZOOM`). The camera clamps to it, so no test can ask for more.
const MAX_ZOOM: f32 = 96.0;

/// The box `MapView::new` seats the world in before the first frame says how big the card
/// is (`views/map.rs:905`), and the box `views/map.rs`'s own tests measure against.
const CARD: (f32, f32) = (1000.0, 470.0);

/// A tall box: 860 × 560, the second real size the layout is checked at. The plate is
/// height-limited here, so the world is wider than the box and the far east and west are
/// cropped — see [`a_tall_plate_crops_the_edges_and_a_pan_brings_them_back`].
const TALL: (f32, f32) = (860.0, 560.0);

/// The tightest a round trip of a whole plate can be asked to close in, in degrees.
///
/// Measured, the worst of the four cameras is 1.525 878 9 × 10⁻⁵ ° — two of f32's own last
/// digits at latitude 90°, where the plate's arithmetic is widest. The ceiling is six times
/// that: enough that a compiler or a platform reordering the two operations cannot fail it,
/// and a quarter of a pixel (0.027 px) at [`MAX_ZOOM`], so a rate that moved at all does.
const EPSILON_DEG: f32 = 1e-4;

// ------------------------------------------------------------------ the view's math

/// The scale at which the plate covers the box, one of the axes exactly: `geo.rs:196`,
/// `Camera::fill`. Nothing is letterboxed, so this is the floor of every zoom.
fn fill(size: (f32, f32)) -> f32 {
    (size.0 / PLATE_W).max(size.1 / PLATE_H)
}

/// Where the plate's middle is drawn: the centre of the box the view allocates
/// (`views/map.rs:858`, `let centre = rect.center()`).
fn centre(size: (f32, f32)) -> Pos2 {
    Pos2::new(size.0 / 2.0, size.1 / 2.0)
}

/// A place as the view paints it: `views/map.rs:773`, call for call —
/// `centre + camera.project(at)`.
///
/// Duplicated on purpose. It is the one line that joins `geo` to the plate, and a test that
/// called the view's own copy would be asking the code whether it agrees with itself.
fn on_plate(camera: &Camera, centre: Pos2, at: Geo) -> Pos2 {
    let point = camera.project(at);
    Pos2::new(centre.x + point.x, centre.y + point.y)
}

/// The grid the round trip is measured on: every 5° of longitude and latitude, both
/// antimeridian edges and both poles included — 73 × 37 = 2 701 places, the whole plate.
fn world_grid() -> Vec<Geo> {
    let mut grid = Vec::with_capacity(73 * 37);
    let mut lat = -90.0_f32;
    while lat <= 90.0 {
        let mut lon = -180.0_f32;
        while lon <= 180.0 {
            grid.push(Geo::new(lon, lat));
            lon += 5.0;
        }
        lat += 5.0;
    }
    grid
}

fn finite(at: Point) -> bool {
    at.x.is_finite() && at.y.is_finite()
}

fn finite_geo(at: Geo) -> bool {
    at.lon.is_finite() && at.lat.is_finite()
}

/// A camera still holds the two promises its type documents: the scale is between the zoom
/// that fills the box and [`MAX_ZOOM`] times it, and the plate covers the box — each axis
/// stopped with its edge on the box's edge, so no air is shown beside the world.
fn settled(camera: &Camera, size: (f32, f32)) -> bool {
    let floor = fill(size);
    let scale = camera.zoom() * floor;
    let slack_x = ((PLATE_W * scale - size.0) / 2.0).max(0.0);
    let slack_y = ((PLATE_H * scale - size.1) / 2.0).max(0.0);
    scale.is_finite()
        && scale >= floor - 1e-3
        && scale <= floor * MAX_ZOOM + 1e-3
        && camera.offset().x.is_finite()
        && camera.offset().y.is_finite()
        && camera.offset().x.abs() <= slack_x + 1e-3
        && camera.offset().y.abs() <= slack_y + 1e-3
}

// ---------------------------------------------------------------------- the tests

/// The round trip closes over the whole plate, at every camera the view can be in.
///
/// Both directions are measured: degrees → pixels → degrees (what a click needs) and
/// pixels → degrees → pixels (what a drag needs). The worst error of either, over 2 701
/// places on four cameras, is printed and must stay under [`EPSILON_DEG`].
#[test]
fn the_round_trip_holds_everywhere_on_the_plate() {
    let grid = world_grid();
    let mut deep = Camera::new(CARD);
    deep.zoom_at(Point::new(0.0, 0.0), MAX_ZOOM);
    let mut moved = Camera::new(TALL);
    moved.zoom_at(Point::new(0.0, 0.0), 8.0);
    moved.pan((97.0, -41.0));
    moved.resize((900.0, 600.0));

    // Each camera is asserted to be the case it is named for, so a change that quietly
    // pinned one of them to the opening zoom cannot shrink this test's coverage.
    assert!((Camera::new(CARD).zoom() - 1.0).abs() < 1e-4, "the card does not open at 1×");
    assert!((Camera::new(TALL).zoom() - 1.0).abs() < 1e-4, "the tall card does not open at 1×");
    assert!(
        (deep.zoom() - MAX_ZOOM).abs() < 1e-2,
        "the deep camera is at {}×, not {MAX_ZOOM}×",
        deep.zoom()
    );
    // `resize` keeps the pixels-per-degree, not the zoom *number* — the number is measured
    // against the new box's fill — so 8× on the tall card reads 7.47× on a 900 × 600 one:
    // 8 · (560/180) = 24.9 px/° against a 900/360 = 2.5 and 600/180 = 3.33 fill.
    assert!(
        (moved.zoom() - 8.0 * fill(TALL) / fill((900.0, 600.0))).abs() < 1e-2
            && moved.offset().x.abs() + moved.offset().y.abs() > 10.0,
        "the moved camera is at {}× with offset {:?}, not {}× and panned",
        moved.zoom(),
        moved.offset(),
        8.0 * fill(TALL) / fill((900.0, 600.0))
    );

    let cameras = [
        ("the card the view opens with", Camera::new(CARD), CARD),
        ("a tall card", Camera::new(TALL), TALL),
        ("the deepest zoom", deep, CARD),
        ("resized, panned, at 8x", moved, (900.0, 600.0)),
    ];

    for (name, camera, size) in cameras {
        assert!(
            settled(&camera, size),
            "{name}: the camera is not settled: zoom {}, offset {:?}",
            camera.zoom(),
            camera.offset()
        );
        let mut worst_degrees = 0.0_f32;
        let mut worst_pixels = 0.0_f32;
        let mut worst_at = Geo::new(0.0, 0.0);
        for at in &grid {
            let pixel = camera.project(*at);
            assert!(
                finite(pixel),
                "{name}: {at:?} projects to {pixel:?}, which is not a pixel"
            );
            let back = camera.unproject(pixel);
            assert!(
                finite_geo(back),
                "{name}: {at:?} came back as {back:?}, which is not a place"
            );
            let error = (back.lon - at.lon).abs().max((back.lat - at.lat).abs());
            if error > worst_degrees {
                worst_degrees = error;
                worst_at = *at;
            }
            // and the other way: a pixel through a place and back
            let there = camera.unproject(pixel);
            let again = camera.project(there);
            worst_pixels = worst_pixels
                .max((again.x - pixel.x).abs())
                .max((again.y - pixel.y).abs());
        }
        println!(
            "{name}: {} places, worst round trip {worst_degrees:e}° (at {worst_at:?}) \
             and {worst_pixels:e} px",
            grid.len()
        );
        assert!(
            worst_degrees <= EPSILON_DEG,
            "{name}: a place came back {worst_degrees}° away from {worst_at:?} — over the \
             {EPSILON_DEG}° the plate allows"
        );
        assert!(
            worst_pixels <= EPSILON_DEG,
            "{name}: a pixel came back {worst_pixels} px away — over the {EPSILON_DEG} px \
             a place is allowed to move"
        );
    }
}

/// The two edges of the plate are one plate-width apart, and each is itself: no wrap.
///
/// This is the property a reader notices at the antimeridian — a ring that wanders across
/// it must not be drawn as a line from one side of the plate to the other — and the reason
/// the projection must be a plain multiplication: the view splits those rings at parse time
/// (`views/map.rs:376`) instead of asking the projection to fold.
#[test]
fn the_plate_edges_are_the_plate_and_not_a_wrap() {
    // The card is width-limited: 360° across 1 000 px, so the antimeridian is on the edge
    // and the poles stick 15 px out of the top and bottom of the plate.
    let card = Camera::new(CARD);
    let scale = fill(CARD);
    assert!(
        (card.zoom() - 1.0).abs() < 1e-4,
        "the card should open at the fill, not at zoom {}",
        card.zoom()
    );
    for (edge, sign) in [(-180.0_f32, -1.0_f32), (180.0, 1.0)] {
        let at = card.project(Geo::new(edge, 0.0));
        assert!(
            (at.x - sign * CARD.0 / 2.0).abs() < 1e-3,
            "the {edge}° meridian is drawn at x = {}, not on the plate's edge {}",
            at.x,
            sign * CARD.0 / 2.0
        );
        let back = card.unproject(at);
        assert!(
            (back.lon - edge).abs() <= EPSILON_DEG,
            "the edge {edge}° came back as {}° — the projection wrapped",
            back.lon
        );
    }
    let west = card.project(Geo::new(-180.0, 0.0));
    let east = card.project(Geo::new(180.0, 0.0));
    assert!(
        ((east.x - west.x) - PLATE_W * scale).abs() < 1e-2,
        "the antimeridian's two ends are {} px apart, not the plate's {} px",
        east.x - west.x,
        PLATE_W * scale
    );
    // The poles: 90° of latitude is half the plate, which is 250 px on a 470 px card — so
    // they are drawn 15 px outside it. The view's own note says the same
    // (`views/map.rs:1115`), and a zoom that letterboxed would put them inside instead.
    for (pole, sign) in [(-90.0_f32, 1.0_f32), (90.0, -1.0)] {
        let at = card.project(Geo::new(0.0, pole));
        // half the plate's height is 90° = 250 px, and the box's is 235: the pole sits
        // 15 px beyond the edge, on its own side.
        let expected = sign * (CARD.1 / 2.0 + 15.0);
        assert!(
            (at.y - expected).abs() < 1e-3,
            "the {pole}° pole is drawn at y = {}, not {expected}",
            at.y
        );
        let back = card.unproject(at);
        assert!(
            (back.lat - pole).abs() <= EPSILON_DEG,
            "the pole {pole}° came back as {}°",
            back.lat
        );
    }
}

/// Every mutator refuses garbage instead of propagating it.
///
/// NaN and infinity reach a camera from three directions that all exist in the window: a
/// trackpad that reports a zero-size window, a gesture whose delta is not a number, and a
/// resize while the renderer has no size yet. `geo` is the last place that can stop them,
/// and it does — the camera stays finite, settled and usable, so the plate keeps drawing.
#[test]
fn garbage_input_leaves_a_finite_settled_camera() {
    // What a box of each shape has to become: `geo.rs:78`, `finite_or_one`, turns anything
    // that is not a number describing a box into 1 px a side — `f32::max(1.0)` already
    // catches zero, negative and NaN (it returns the finite operand), and the helper is
    // what catches the infinity, which `max(1.0)` does not.
    let sizes = [
        ("zero", (0.0_f32, 0.0_f32), (1.0, 1.0)),
        ("negative", (-10.0, -10.0), (1.0, 1.0)),
        ("NaN", (f32::NAN, f32::NAN), (1.0, 1.0)),
        ("NaN width, real height", (f32::NAN, 5.0), (1.0, 5.0)),
        ("infinite", (f32::INFINITY, f32::INFINITY), (1.0, 1.0)),
        ("infinite width, real height", (f32::INFINITY, 400.0), (1.0, 400.0)),
        ("absurd but finite", (1e9, 1e9), (1e9, 1e9)),
    ];
    for (name, size, kept) in sizes {
        let camera = Camera::new(size);
        println!(
            "{name} box {size:?} → kept {kept:?}: zoom {}, offset {:?}",
            camera.zoom(),
            camera.offset()
        );
        // Not merely finite: settled and usable at the box `finite_or_one` left behind.
        assert!(
            settled(&camera, kept),
            "{name} box {size:?}: left the camera at zoom {} offset {:?}, which is not a \
             settled camera in the {kept:?} the helper should have kept",
            camera.zoom(),
            camera.offset()
        );
        // The zoom is a ratio against that box, so it reads 1× when the scale is the fill —
        // which is what `resize` asks for on a box it has just sanitised.
        assert!(
            (camera.zoom() - 1.0).abs() < 1e-3,
            "{name} box {size:?}: opened at {}× rather than at the fill of {kept:?}",
            camera.zoom()
        );
        for at in world_grid() {
            assert!(
                finite(camera.project(at)),
                "{name} box {size:?}: {at:?} projects to {:?}",
                camera.project(at)
            );
        }
    }

    // The infinite box is the case the helper exists for, checked on its own so the reason
    // survives the table: `geo.rs:78` (`finite_or_one`) is what stops `∞/∞` — the infinity
    // is the one size `max(1.0)` passes through, and without the helper it made the fill
    // infinite, the scale infinite and every projection NaN.
    let infinite = Camera::new((f32::INFINITY, f32::INFINITY));
    assert!(
        infinite.zoom().is_finite() && settled(&infinite, (1.0, 1.0)),
        "an infinitely wide box left the camera at zoom {} — `finite_or_one` (geo.rs:78) \
         is not keeping 1 px a side",
        infinite.zoom()
    );

    // Gestures: a NaN delta must not move the plate, an absurd one must land inside the
    // slack the plate has, and a factor of NaN, 0, -1 or 1e9 must not break the zoom.
    let mut camera = Camera::new(CARD);
    let before = camera;
    for delta in [
        (f32::NAN, 0.0_f32),
        (0.0, f32::INFINITY),
        (f32::NEG_INFINITY, 1.0),
    ] {
        camera.pan(delta);
        assert_eq!(
            camera, before,
            "a pan of {delta:?} moved the plate to {:?}",
            camera.offset()
        );
    }
    for factor in [f32::NAN, f32::INFINITY, 0.0, -1.0, -1e9] {
        camera.zoom_at(Point::new(3.0, 4.0), factor);
        assert!(
            settled(&camera, CARD),
            "a zoom by {factor} left the camera unsettled: zoom {}, offset {:?}",
            camera.zoom(),
            camera.offset()
        );
    }
    camera.zoom_at(Point::new(f32::NAN, 0.0), 2.0);
    assert!(settled(&camera, CARD), "a zoom about a NaN anchor broke the camera");

    camera.pan((1e12, -1e12));
    assert!(
        settled(&camera, CARD),
        "a pan of 1e12 px left the camera at {:?}, outside the plate's slack",
        camera.offset()
    );
    camera.zoom_at(Point::new(0.0, 0.0), 1e9);
    assert!(
        settled(&camera, CARD) && (camera.zoom() - MAX_ZOOM).abs() < 1e-2,
        "a zoom by 1e9 reached {}, over the {MAX_ZOOM}× ceiling",
        camera.zoom()
    );
    camera.resize((f32::NAN, 0.0));
    assert!(
        settled(&camera, CARD),
        "a resize to a NaN box left the camera at zoom {} offset {:?}",
        camera.zoom(),
        camera.offset()
    );

    // And with the camera still holding garbage history, the grid projects finite.
    for at in world_grid() {
        assert!(
            finite(camera.project(at)),
            "{at:?} projects to {:?} after the garbage",
            camera.project(at)
        );
    }
}

/// No coordinate the parser keeps can project to an infinity, at any zoom.
///
/// `views/map.rs:364` drops a coordinate with `|lon| > 180`, `|lat| > 90` or a
/// non-finite part before it ever becomes a [`Geo`], which is what keeps the arithmetic
/// below in range. This test pins both halves: the kept range never overflows, at the
/// deepest zoom the camera can hold; and the boundary where f32 *would* overflow is far
/// outside it, so the guard is not a coincidence of today's zoom ceiling.
#[test]
fn a_coordinate_the_parser_keeps_never_projects_to_infinity() {
    let mut deepest = Camera::new(CARD);
    deepest.zoom_at(Point::new(0.0, 0.0), MAX_ZOOM);
    let mut measured = 0usize;
    for lat in [-90.0_f32, -45.0, 0.0, 45.0, 90.0] {
        for lon in [-180.0_f32, -179.999, 0.0, 179.999, 180.0] {
            let at = deepest.project(Geo::new(lon, lat));
            assert!(
                finite(at),
                "{lon}°, {lat}° projects to {at:?} at the deepest zoom"
            );
            assert!(
                finite_geo(deepest.unproject(at)),
                "{lon}°, {lat}° unprojects to {:?}",
                deepest.unproject(at)
            );
            measured += 1;
        }
    }
    // The overflow the guard prevents: f32's own ceiling, where the multiplication leaves
    // the type. Measured here rather than assumed, so the distance from the parser's bound
    // is a number in the log and not a claim in a comment.
    let scale = fill(CARD);
    let overflows_above = f32::MAX / scale / MAX_ZOOM;
    println!(
        "{measured} kept coordinates project finite at {MAX_ZOOM}×; the projection would \
         overflow at |lon| > {overflows_above:e}°, {} times the parser's 180°",
        overflows_above / 180.0
    );
    assert!(
        (deepest.project(Geo::new(overflows_above * 1.01, 0.0)).x).is_infinite(),
        "the projection did not overflow at {}°, so the measured boundary is wrong",
        overflows_above * 1.01
    );

    // Off the plate is not the same as out of range: the projection is a multiplication and
    // does not clamp, so 400° lands 111 px past the card's right edge at the opening zoom —
    // finite, outside the box, and *unreachable*, because the parser drops it first. A NaN
    // coordinate would project to NaN; that is the parser's job to refuse, which
    // [`a_coordinate_off_the_planet_is_dropped_before_it_is_drawn`] checks at the entry
    // point rather than here.
    let off_plate = Camera::new(CARD).project(Geo::new(400.0, 0.0));
    println!("400° projects to x = {:.1} on a 1 000 px card (400 − 180 = 111 px past it)", off_plate.x);
    assert!(
        finite(off_plate) && off_plate.x > CARD.0 / 2.0,
        "400° projects to {off_plate:?}: finite and off the plate is the contract"
    );
}

/// Six cities, on the pixels the view draws them at.
///
/// The numbers are worked out by hand, from the two promises the plate makes: a degree of
/// longitude and a degree of latitude are the same number of pixels (the card's fill,
/// 1 000/360 = 2.7778), and the world's middle is the box's middle (500, 235). So a place
/// at `(lon, lat)` is drawn at `(500 + lon·2.7778, 235 − lat·2.7778)` — east is right,
/// north is *up*, which is the one sign that costs a letter in the source and a whole
/// flipped world on screen.
///
/// Half a pixel of slack, which is under the 1 px hairline a coastline is drawn with.
#[test]
fn known_cities_land_on_the_pixels_the_view_draws_them_at() {
    let camera = Camera::new(CARD);
    let middle = centre(CARD);
    // (city, lon, lat, x, y) — the pixels computed from the rate above, not from the code.
    let cities = [
        ("Berlin", 13.404_954_f32, 52.520_008_f32, 537.236, 89.111),
        ("Tokyo", 139.691_7, 35.689_5, 888.033, 135.862),
        ("New York", -74.006_0, 40.712_8, 294.428, 121.909),
        ("Sydney", 151.209_3, -33.868_8, 920.026, 329.080),
        ("Lagos", 3.379_2, 6.524_4, 509.387, 216.877),
        ("Reykjavik", -21.942_6, 64.146_6, 439.048, 56.815),
    ];
    for (name, lon, lat, x, y) in cities {
        let at = Geo::new(lon, lat);
        let drawn = on_plate(&camera, middle, at);
        println!("{name}: {lon}°, {lat}° → {drawn:?}");
        assert!(
            (drawn.x - x).abs() < 0.5 && (drawn.y - y).abs() < 0.5,
            "{name} is drawn at {drawn:?}, not ({x}, {y}) — the plate's rate or its origin \
             moved"
        );
        assert!(
            drawn.x >= 0.0 && drawn.x <= CARD.0 && drawn.y >= 0.0 && drawn.y <= CARD.1,
            "{name} at {drawn:?} is off the {CARD:?} card"
        );
        // The same place back from the pixel the view painted it at: a click on the dot
        // has to name the city.
        let back = camera.unproject(Point::new(drawn.x - middle.x, drawn.y - middle.y));
        assert!(
            (back.lon - lon).abs() <= EPSILON_DEG && (back.lat - lat).abs() <= EPSILON_DEG,
            "clicking {name}'s dot says {back:?}, not {lon}°, {lat}°"
        );
    }
}

/// A tall plate crops the plate's edges, and the slack in the clamp is what brings them
/// back — no place is lost, only off screen until the reader pans.
///
/// At 860 × 560 the fill is height-limited (560/180 = 3.1111 against 860/360 = 2.3889), so
/// the world is 360 × 3.1111 = 1 120 px wide in an 860 px box: 130 px of it on each side is
/// outside. Measured, that puts Tokyo (x = 900.4) and Sydney (x = 964.9) past the right
/// edge at the opening zoom. The camera's horizontal slack is exactly those 130 px, so one
/// pan of 130 px puts them back — which is the difference between "the plate is larger than
/// the window" and "the map lost a continent".
#[test]
fn a_tall_plate_crops_the_edges_and_a_pan_brings_them_back() {
    let camera = Camera::new(TALL);
    let middle = centre(TALL);
    let scale = fill(TALL);
    let slack = (PLATE_W * scale - TALL.0) / 2.0;
    assert!(
        (slack - 130.0).abs() < 1e-2,
        "the tall plate crops {slack} px a side, not 130"
    );

    let far_east = on_plate(&camera, middle, Geo::new(151.209_3, -33.868_8));
    let tokyo = on_plate(&camera, middle, Geo::new(139.691_7, 35.689_5));
    println!(
        "at 860 × 560: Sydney x = {:.1}, Tokyo x = {:.1}, in an 860 px box; {slack:.0} px of \
         slack on each side",
        far_east.x, tokyo.x
    );
    assert!(
        tokyo.x > TALL.0,
        "Tokyo is at {}, inside the box — the crop measured at 860 px wide moved",
        tokyo.x
    );
    assert!(
        far_east.x > TALL.0,
        "Sydney is at {}, inside the box — the plate is not width-cropped as measured",
        far_east.x
    );

    // Pan by the whole slack, leftwards — the plate moves left under the pointer, which is
    // what brings a place drawn past the right edge back into the box.
    let mut panned = camera;
    panned.pan((-slack, 0.0));
    let again = on_plate(&panned, middle, Geo::new(151.209_3, -33.868_8));
    assert!(
        again.x <= TALL.0 && again.x >= 0.0,
        "after a {slack} px pan Sydney is at {} — the slack did not bring it back",
        again.x
    );
    // And the clamp stops there: a further pan of the same size is refused, so no air
    // ever appears beside the world.
    panned.pan((-slack, 0.0));
    assert!(
        settled(&panned, TALL),
        "the clamp let the plate leave the box: offset {:?}",
        panned.offset()
    );
}

/// A coordinate off the planet is dropped before it is drawn, not projected into nonsense.
///
/// This is the guard the projection relies on (`views/map.rs:364`): a file with a longitude
/// of 400, a latitude of 200, a NaN and an infinity loses those vertices and keeps the rest.
/// Hostile coordinates are cheap to write and expensive to debug from a screenshot, so the
/// entry point is tested rather than trusted.
#[test]
fn a_coordinate_off_the_planet_is_dropped_before_it_is_drawn() {
    // One ring: two good points, three hostile ones. Kept: (13.4 52.5) and (13.5 52.6).
    let text = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[
            [13.4, 52.5], [400.0, 52.5], [13.5, 52.6],
            [13.45, 200.0], [1e40, 1e40], [13.4, 52.5]
        ]]}}]}"#;
    let world = World::from_geojson(text);
    println!(
        "hostile ring: {} coastlines, {} points kept",
        world.lines(),
        world.points()
    );
    assert_eq!(
        world.points(),
        3,
        "the parser kept {} points: the closing point and the two in-range ones are all it \
         may keep out of six",
        world.points()
    );
    assert_eq!(world.lines(), 1, "one ring in, {} out", world.lines());

    // Every kept point is on the plate, and draws finite pixels at any zoom.
    let ring = [
        Geo::new(13.4, 52.5),
        Geo::new(13.5, 52.6),
        Geo::new(13.4, 52.5),
    ];
    let mut deepest = Camera::new(CARD);
    deepest.zoom_at(Point::new(0.0, 0.0), MAX_ZOOM);
    for at in ring {
        assert!(
            at.lon.abs() <= 180.0 && at.lat.abs() <= 90.0,
            "{at:?} survived the guard"
        );
        assert!(finite(deepest.project(at)), "{at:?} projects to an infinity");
    }

    // Garbage in, an empty world out — never a panic.
    for junk in ["", "not json", "[]", r#"{"features":[]}"#] {
        let empty = World::from_geojson(junk);
        assert_eq!(
            (empty.lines(), empty.points()),
            (0, 0),
            "{junk:?} produced {} coastlines and {} points",
            empty.lines(),
            empty.points()
        );
    }
}

// ------------------------------------------- the opening zoom: fill, or contain?

/// The five window sizes the question is asked at: the smallest the layout must survive, two
/// ordinary laptops, a desktop, and an ultrawide. Aspect ratios 1.54, 1.34, 1.60, 1.78 and
/// 2.39 against the plate's 2.0, so two of them crop left and right and three crop top and
/// bottom under fill.
const WINDOWS: [(f32, f32); 5] = [
    (860.0, 560.0),
    (1100.0, 820.0),
    (1440.0, 900.0),
    (1920.0, 1080.0),
    (3440.0, 1440.0),
];

/// Every story in the shipped archive that carries a coordinate, read read-only, or `None`
/// when the checkout has no archive — the pattern `tests/place_stories.rs:51` uses: a bare
/// clone has nothing to answer, and the test says so in one sentence and passes.
///
/// The window is twenty years rather than the app's 72 h on purpose: the question is *where*
/// the news sits, and a window measured against today's clock would answer differently every
/// week as the shipped file ages.
fn shipped_pins() -> Option<Vec<Pin>> {
    let path = assets::archive();
    if !path.is_file() {
        eprintln!("skipped: no archive at {} — nothing to measure", path.display());
        return None;
    }
    let archive = match Archive::open(&path) {
        Ok(archive) => archive,
        Err(sentence) => {
            eprintln!("skipped: {sentence}");
            return None;
        }
    };
    Some(archive.pins(20 * 365 * 24, 100_000))
}

/// The scale a **fill** opening draws at: the plate covers the box, one axis exactly
/// (`geo.rs:196`).
fn fill_scale(size: (f32, f32)) -> f32 {
    (size.0 / PLATE_W).max(size.1 / PLATE_H)
}

/// The scale a **contain** opening would draw at: the whole plate inside the box, one axis
/// exactly. Spelled out here rather than built from [`Camera`], because `Camera` cannot hold
/// it — `fill()` is a `max` and `settle()` clamps the scale *up* to it (`geo.rs:196`,
/// `geo.rs:206`), so every camera the app can construct covers its box.
fn contain_scale(size: (f32, f32)) -> f32 {
    (size.0 / PLATE_W).min(size.1 / PLATE_H)
}

/// A place as a plate pixel at a given scale, both modes being the same equirectangular
/// arithmetic with the world's middle on the box's middle.
fn plate_pixel(size: (f32, f32), scale: f32, at: Geo) -> Pos2 {
    Pos2::new(
        size.0 / 2.0 + at.lon * scale,
        size.1 / 2.0 - at.lat * scale,
    )
}

/// What one opening zoom shows: the scale, the plate it leaves empty, the dots it draws, and
/// the news it puts off it.
struct Opening {
    scale: f32,
    /// Pixels of the box with no world in them, on the loose axis — 0 under fill by
    /// definition, because the plate covers the box.
    dead_band: f32,
    /// The same band as a share of the box's area: what the reader pays for contain.
    dead_share: f32,
    dots: usize,
    places: usize,
    places_off: usize,
    stories: i64,
    stories_off: i64,
    /// The half-range of longitude and latitude the box shows, clipped to the world's own.
    lon_seen: f32,
    lat_seen: f32,
}

/// Measure one mode: project every pin, cluster it with the view's own [`cluster`] and the
/// view's own [`CLUSTER_PX`] (which drops the pins outside the box, exactly as a frame does),
/// and count what fell off.
fn opening(size: (f32, f32), scale: f32, pins: &[Pin]) -> Opening {
    let box_ = Rect::from_min_size(Pos2::ZERO, Vec2::new(size.0, size.1));
    let at = |pin: &Pin| plate_pixel(size, scale, Geo::new(pin.lon as f32, pin.lat as f32));
    let dots = cluster(pins, at, box_, CLUSTER_PX);
    let places: usize = dots.iter().map(|dot| dot.places).sum();
    let stories: i64 = dots.iter().map(|dot| dot.stories).sum();
    let all_stories: i64 = pins.iter().map(|pin| pin.count).sum();
    let short_x = (size.0 - PLATE_W * scale).max(0.0);
    let short_y = (size.1 - PLATE_H * scale).max(0.0);
    Opening {
        scale,
        dead_band: short_x + short_y,
        dead_share: (short_x * size.1 + short_y * size.0) / (size.0 * size.1),
        dots: dots.len(),
        places,
        places_off: pins.len() - places,
        stories,
        stories_off: all_stories - stories,
        lon_seen: ((size.0 / 2.0) / scale).min(180.0),
        lat_seen: ((size.1 / 2.0) / scale).min(90.0),
    }
}

/// Fill or contain at the opening zoom, measured on the shipped archive: five window sizes,
/// both modes, the news each puts off the plate, and the plate contain leaves empty.
///
/// The rows print; four claims are asserted so the table cannot stop meaning what it says:
///
/// * **fill never leaves a dead band** — half a pixel of arithmetic slack, no more: the
///   plate covers the box, which is the rule `geo.rs:196` exists to keep;
/// * **contain never puts a place off the plate** — the whole world is inside the box;
/// * **contain's price is real** — it leaves a band at every one of the five sizes, so the
///   comparison is never between two identical drawings;
/// * **fill's worst crop is small** — under 10% of the stories, at whichever of the five
///   windows turns out to be the worst.
#[test]
fn fill_or_contain_at_the_opening_zoom() {
    let Some(pins) = shipped_pins() else {
        return;
    };
    let all_stories: i64 = pins.iter().map(|pin| pin.count).sum();
    println!(
        "the shipped archive: {all_stories} stories in {} places with a coordinate",
        pins.len()
    );
    println!(
        "window       mode     px/deg  dead band  of area   dots  places  stories  off-plate  \
         sees (lon ±, lat ±)"
    );

    let mut worst_fill: Option<(f32, f32, Opening)> = None;
    for (width, height) in WINDOWS {
        let size = (width, height);
        for (mode, scale) in [
            ("fill", fill_scale(size)),
            ("contain", contain_scale(size)),
        ] {
            let measured = opening(size, scale, &pins);
            println!(
                "{width:>4}x{height:<5} {mode:<8} {scale:>6.3}  {band:>6.0}px {share:>6.1}%  \
                 {dots:>5}  {places:>6}  {stories:>7}  {pairs:>9}  {lon:>6.1}°, {lat:>5.1}°",
                band = measured.dead_band,
                share = measured.dead_share * 100.0,
                dots = measured.dots,
                places = measured.places,
                stories = measured.stories,
                pairs = format!("{}/{}", measured.places_off, measured.stories_off),
                lon = measured.lon_seen,
                lat = measured.lat_seen,
            );

            if mode == "fill" {
                assert!(
                    measured.dead_band < 0.5,
                    "fill left a {:.2} px dead band at {width}x{height} — the plate does not \
                     cover its box",
                    measured.dead_band
                );
                if worst_fill
                    .as_ref()
                    .is_none_or(|(_, _, other)| measured.stories_off > other.stories_off)
                {
                    worst_fill = Some((width, height, measured));
                }
            } else {
                assert_eq!(
                    (measured.places_off, measured.stories_off),
                    (0, 0),
                    "contain put {} places and {} stories off the plate at {width}x{height}, \
                     which is the one thing it is there to prevent",
                    measured.places_off,
                    measured.stories_off
                );
                assert!(
                    measured.dead_share > 0.02,
                    "contain left {:.1}% of the window empty at {width}x{height} — too little \
                     for this comparison to mean anything",
                    measured.dead_share * 100.0
                );
            }
        }
    }

    let (width, height, worst) = worst_fill.expect("the five fill rows were measured");
    let contain = opening((width, height), contain_scale((width, height)), &pins);
    println!(
        "worst fill crop is at {width}x{height}: {} of {all_stories} stories ({:.1}%) in {} of \
         {} places, off by {:.0}° of longitude a side — where contain instead draws the same \
         world at {:.0}% of the scale, with {:.0} px of empty plate ({:.1}% of the window) and \
         {} dots against fill's {}",
        worst.stories_off,
        100.0 * worst.stories_off as f32 / all_stories as f32,
        worst.places_off,
        pins.len(),
        180.0 - worst.lon_seen,
        100.0 * contain.scale / worst.scale,
        contain.dead_band,
        contain.dead_share * 100.0,
        contain.dots,
        worst.dots
    );
    assert!(
        (worst.stories_off as f32) < 0.10 * all_stories as f32,
        "fill crops {} of {all_stories} stories at {width}x{height} — over the 10% this \
         trade-off was decided on",
        worst.stories_off
    );
    assert!(
        worst.stories_off > 0,
        "fill crops nothing at {width}x{height} — the premise of the question no longer holds"
    );
}
