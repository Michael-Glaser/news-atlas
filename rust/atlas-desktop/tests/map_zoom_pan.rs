//! The map's smoothness rules, driven through the modules that hold them.
//!
//! Six complaints, one file: a scroll or a pinch must accumulate a *fraction* of a zoom
//! and spend it over frames (one notch is a percent, not a jump); the zoom stops at both
//! ends and pressing against an end moves nothing; a drag moves the picture by exactly the
//! distance the pointer moved, in screen points, whatever the zoom; nothing a gesture does
//! may put a place off the projection — a coordinate is drawn where the projection says and
//! comes back as itself, and the plate never walks off Earth; and a zoom is applied around
//! the pointer, never around the middle of the plate. The seventh pins what a click has to
//! answer: the index of the pin under it, so the page above the plate can open that story.
//!
//! Three owners, one rule each. [`atlas_desktop::gesture`] accumulates: an event only adds
//! to what is pending, and one frame spends a bounded share of it. [`atlas_desktop::geo`]
//! owns the camera — `project = centre + (lon · k, −lat · k)`, equirectangular, north up,
//! with the scale and the plate's own edges clamped after every move. [`views::map`] owns
//! the motion that eases the plate toward its target one capped step at a time, and the
//! dots a click lands on. This file only calls their public API, so a change inside either
//! is a change in one place.
//!
//! Two coordinate conventions are easy to mix up here, and each test says which it uses:
//! a [`Gesture`] anchor is a **screen position** (`Pos2`, the pointer's own), while a
//! [`Motion`] anchor is a [`Point`] measured **from the box's centre** — which is what the
//! view passes, and what this file converts with [`view`].
//!
//! The tests that print a number are the measurements the numbers exist for: the zoom a
//! notch asks for, the zoom a burst lands at, the degrees the plate's middle travels under
//! a pointer zoom, and how deep a long drag-and-zoom run gets.

use std::path::Path;

use egui::{pos2, vec2, Pos2, Rect, Vec2};

use atlas_desktop::data::Pin;
use atlas_desktop::geo::{Camera, Geo, Point};
use atlas_desktop::gesture::{
    zoom_about, Gesture, PINCH_EVENT_MAX, SLOP, WHEEL_EVENT_MAX, WHEEL_GAIN, ZOOM_PENDING_MAX,
    ZOOM_RATE,
};
use atlas_desktop::views::map::{MapView, Motion, ZOOM_STEP_MAX};

/// The plate the map card allocates: 1 000 × 470 pixels, wider than the world's 2:1, so at
/// the opening zoom the width is exactly filled and the poles sit 15 px outside the box.
const CARD: (f32, f32) = (1000.0, 470.0);

/// The deepest zoom, in multiples of the fit: `geo::Camera::zoom` reads 1 at the fit and 96
/// at the end. The constant behind it is private to `geo` on purpose (a number a view could
/// reach for is a number two views can disagree about), so it is written here once, and
/// the ends below are *reached* by zooming rather than assumed.
const DEEPEST: f32 = 96.0;

fn plate() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), vec2(CARD.0, CARD.1))
}

fn centre() -> Pos2 {
    plate().center()
}

/// A screen position as `geo` measures it: from the box's centre, `y` down.
fn view(at: Pos2) -> Point {
    Point::new(at.x - centre().x, at.y - centre().y)
}

/// Where a camera draws a place, on the plate.
fn screen(camera: &Camera, at: Geo) -> Pos2 {
    let point = camera.project(at);
    pos2(centre().x + point.x, centre().y + point.y)
}

/// The place under a screen position — the inverse, and what a click on the plate means.
fn place(camera: &Camera, at: Pos2) -> Geo {
    camera.unproject(view(at))
}

/// Where the pointer is, a hundred points off the middle of the plate. "Zoom where I am
/// looking" and "zoom the plate" only differ off centre, so every test comes here.
fn pointer() -> Pos2 {
    centre() + vec2(120.0, -70.0)
}

/// Somewhere a place can be: both halves of the world, both sides of the equator, the two
/// poles and both edges of the antimeridian.
const PLACES: [(f32, f32); 6] = [
    (-180.0, -90.0),
    (-179.9, 0.0),
    (0.0, 0.0),
    (13.4, 52.5),
    (179.9, 89.9),
    (180.0, 90.0),
];

/// Has `camera` reached `target`? The same test `views::map` uses to stop easing — a fifth
/// of a pixel of pan and a quarter of a percent of zoom — written here because it is not
/// public and a test that cannot tell "arrived" from "nearly" cannot see a stall.
fn settled(camera: &Camera, target: &Camera) -> bool {
    let (here, there) = (camera.offset(), target.offset());
    (here.x - there.x).abs() < 0.2
        && (here.y - there.y).abs() < 0.2
        && (camera.zoom() - target.zoom()).abs() <= target.zoom() * 0.0025
}

/// Spend a gesture the way a window does — one `step` per frame — and answer the zoom it
/// applied in log units, and the frames it took.
fn spend(gesture: &mut Gesture, dt: f32) -> (f32, usize) {
    let mut log = 0.0;
    let mut frames = 0;
    loop {
        let step = gesture.step(dt);
        log += step.zoom.ln();
        frames += 1;
        if !step.running {
            return (log, frames);
        }
        assert!(frames < 2_000, "a gesture must be spent, not kept: {log} so far");
    }
}

// ------------------------------------------------------------------ accumulation

/// The first complaint: one notch of a wheel must be a percent of a zoom, and the
/// trackpad's burst of events must be spent over frames instead of landing in one jump.
/// A single event never moves a camera at all — it adds to what is pending.
#[test]
fn a_notch_is_a_fraction_of_a_zoom_and_a_burst_is_spent_over_frames() {
    let at = pointer();

    // One notch of ten points is 1.5% — the gain's own fraction — and it is anchored at
    // the pointer rather than at the middle of the plate.
    let mut wheel = Gesture::default();
    wheel.zoom(10.0, 1.0, at);
    let first = wheel.step(1.0 / 60.0);
    assert!(
        (first.zoom - (10.0 * WHEEL_GAIN).exp()).abs() < 1e-6,
        "one notch applied ×{}, not e^(10 · {WHEEL_GAIN})",
        first.zoom
    );
    assert!(first.zoom < 1.02, "one notch is a percent, not a step: ×{}", first.zoom);
    assert_eq!(first.anchor, at, "the zoom is anchored where the pointer is");

    // Fractions of a point count: the accumulator is a float, not a notch counter.
    let mut fine = Gesture::default();
    fine.zoom(3.7, 1.0, at);
    let (log, frames) = spend(&mut fine, 1.0 / 60.0);
    assert!((log - 3.7 * WHEEL_GAIN).abs() < 1e-5, "3.7 points of scroll became {log}");
    // the frame that spends the last share still says "running", so the window paints one
    // more frame that applies nothing: two frames, not a jump and not a stall
    assert_eq!(frames, 2, "a fraction of a notch is one small step");

    // Twenty events between two frames — what a trackpad hands a window. Each event is
    // clamped, the total is clamped, and the frame still applies only its own share.
    let mut burst = Gesture::default();
    for _ in 0..20 {
        burst.zoom(100_000.0, 1.0, at);
    }
    let first = burst.step(1.0 / 60.0);
    let per_frame = ZOOM_RATE / 60.0;
    assert!(
        first.zoom.ln() <= per_frame + 1e-5,
        "one frame of a burst applied {} log units, more than the {} it may",
        first.zoom.ln(),
        per_frame
    );
    assert!(first.zoom < 1.25, "the first frame of a twenty-event burst is ×{}", first.zoom);
    let (rest, frames) = spend(&mut burst, 1.0 / 60.0);
    let total = first.zoom.ln() + rest;
    assert!(
        total <= ZOOM_PENDING_MAX + 1e-4,
        "the pending zoom was not bounded: {total} log units (×{})",
        total.exp()
    );
    assert!(frames > 5, "a burst of twenty events took {frames} frames");
    assert!(total.exp() > 8.0, "and the burst did land: ×{}", total.exp());

    // One page-scroll event and one enormous pinch: each is one event's worth, not a jump
    // to wherever the device said it was.
    let mut page = Gesture::default();
    page.zoom(100_000.0, 1.0, at);
    let (log, _) = spend(&mut page, 1.0 / 60.0);
    assert!((log - WHEEL_EVENT_MAX).abs() < 1e-5, "one wheel event asked for {log}");
    let mut pinch = Gesture::default();
    pinch.zoom(0.0, 1e6, at);
    let (log, _) = spend(&mut pinch, 1.0 / 60.0);
    assert!((log - PINCH_EVENT_MAX).abs() < 1e-5, "one pinch event asked for {log}");

    // egui reports a ctrl-scroll in both readings: one gesture must be one zoom, or the
    // same pinch is spent twice.
    let mut both = Gesture::default();
    both.zoom(100.0, 1.2, at);
    let (log, _) = spend(&mut both, 1.0 / 60.0);
    assert!(
        (log - 1.2_f32.ln()).abs() < 1e-5,
        "the same pinch was counted twice: {log} instead of {}",
        1.2_f32.ln()
    );

    println!(
        "one notch ×{:.4}, a twenty-event burst ×{:.2} over {} frames",
        (10.0_f32 * WHEEL_GAIN).exp(),
        total.exp(),
        frames
    );
}

/// The same rule on the plate's own path: the queued zoom is spent a capped step per
/// frame, and a drag drops what was queued — dragging means "not that zoom".
#[test]
fn the_plate_spends_a_queued_zoom_a_step_at_a_time() {
    let anchor = view(pointer());
    let start = Camera::new(CARD);

    let mut motion = Motion::new(start);
    for _ in 0..20 {
        motion.zoom_by(0.11, anchor);
    }
    let mut camera = start;
    let (first, moving) = motion.step(camera, 1.0 / 60.0);
    assert!(moving, "a pending gesture keeps the loop awake");
    let applied = (first.zoom() / start.zoom()).ln();
    assert!(
        applied > 0.0 && applied < ZOOM_STEP_MAX + 1e-6,
        "one frame applied {applied} log units, more than the {ZOOM_STEP_MAX} a frame may"
    );
    assert!(
        first.zoom() / start.zoom() < 1.5,
        "the first frame jumped to ×{}",
        first.zoom() / start.zoom()
    );

    camera = first;
    let mut frames = 1;
    while frames < 240 && (motion.is_moving() || !settled(&camera, &motion.target)) {
        camera = motion.step(camera, 1.0 / 60.0).0;
        frames += 1;
    }
    assert!(frames > 2, "a twenty-event gesture is not one frame of zoom: {frames} frames");
    assert!(
        settled(&camera, &motion.target),
        "the ease stopped {:.2} px short of its target",
        (camera.offset().x - motion.target.offset().x).abs()
    );
    println!(
        "a 2.2 burst: ×{:.3} in the first frame, ×{:.3} when it lands, after {frames} frames",
        first.zoom() / start.zoom(),
        camera.zoom() / start.zoom()
    );

    // A drag cancels what was queued, and moves the target 1:1 — pixels, so the zoom does
    // not touch the distance the pointer travelled. Zoomed in first: at the fit the plate
    // has no slack, and `geo` is right to swallow a drag there rather than show air.
    let mut start = Camera::new(CARD);
    start.zoom_at(Point::new(0.0, 0.0), 4.0);
    let mut motion = Motion::new(start);
    motion.zoom_by(0.3, anchor);
    assert!(motion.is_moving(), "there was a zoom queued");
    let by = (37.5_f32, -21.25_f32);
    let target = motion.dragged(by, 0.0);
    assert_eq!(
        target.offset(),
        Point::new(start.offset().x + by.0, start.offset().y + by.1),
        "the target is the drag, exactly"
    );
    assert_eq!(target.zoom(), start.zoom(), "the drag does not change the zoom");
    assert!(!motion.is_moving(), "a drag cancels the queued zoom");
}

// ------------------------------------------------------------------------ clamps

/// The zoom stops at both ends, and pressing against an end moves nothing — not a little,
/// and not past it. The accumulator's bounds (`k`, handed to it by the view) and the
/// camera's own clamp are the same rule asked twice.
#[test]
fn the_zoom_stops_at_both_ends_and_presses_against_them() {
    let (centre_at, anchor) = (centre(), pointer());
    // the accumulator clamps `k` to whatever the view hands it; the map's own pair is
    // "the fit" and "ninety-six times the fit", which the camera test below reaches instead
    let bounds = (2.0_f32, 200.0_f32);

    let (mut pan, mut k) = (Vec2::ZERO, bounds.0);
    for step in 0..80 {
        let (next_pan, next_k) = zoom_about(pan, k, 2.0, anchor, centre_at, bounds);
        assert!(
            next_k >= bounds.0 && next_k <= bounds.1,
            "step {step} zoomed to k = {next_k}"
        );
        assert!(next_k >= k, "zooming in never zooms out (k = {next_k})");
        (pan, k) = (next_pan, next_k);
    }
    assert_eq!(k, bounds.1, "the ceiling is reached and held");
    assert_eq!(
        zoom_about(pan, k, 2.0, anchor, centre_at, bounds),
        (pan, k),
        "a zoom past the ceiling is not a pan either"
    );
    for step in 0..80 {
        let (next_pan, next_k) = zoom_about(pan, k, 0.5, anchor, centre_at, bounds);
        assert!(
            next_k >= bounds.0 && next_k <= bounds.1,
            "step {step} zoomed out to k = {next_k}"
        );
        assert!(next_k <= k, "zooming out never zooms in (k = {next_k})");
        (pan, k) = (next_pan, next_k);
    }
    assert_eq!(k, bounds.0, "and the floor is reached and held");
    assert_eq!(zoom_about(pan, k, 0.5, anchor, centre_at, bounds), (pan, k));

    // a broken device cannot poison the camera
    assert_eq!(
        zoom_about(pan, 2.0, f32::NAN, anchor, centre_at, bounds),
        (pan, 2.0),
        "a NaN factor is not a zoom"
    );
    assert_eq!(zoom_about(pan, 2.0, 0.0, anchor, centre_at, bounds), (pan, 2.0));
    assert_eq!(zoom_about(pan, -1.0, 2.0, anchor, centre_at, bounds), (pan, -1.0));

    // the camera's own ends, reached by zooming rather than by naming a constant
    let mut camera = Camera::new(CARD);
    for _ in 0..80 {
        camera.zoom_at(view(anchor), 2.0);
    }
    assert_eq!(camera.zoom(), DEEPEST, "the camera stops at the ceiling");
    let held = camera;
    camera.zoom_at(view(anchor), 2.0);
    assert_eq!(camera, held, "a zoom past the ceiling moves nothing at all");

    let mut camera = Camera::new(CARD);
    for _ in 0..80 {
        camera.zoom_at(view(anchor), 0.5);
    }
    assert_eq!(camera.zoom(), 1.0, "and the floor is the fit itself");
    // the fit means "the plate fills the box on the tighter axis": 1 000 px of world across
    assert!(
        (camera.project(Geo::new(-180.0, 0.0)).x + CARD.0 / 2.0).abs() < 1e-2,
        "at the floor the world's west edge is {} px from the middle, not {}",
        camera.project(Geo::new(-180.0, 0.0)).x,
        -CARD.0 / 2.0
    );
    let held = camera;
    camera.zoom_at(view(anchor), 0.5);
    assert_eq!(camera, held, "a zoom past the floor moves nothing at all");

    // and nothing a mouse or a trackpad can report gets past the guard
    let mut camera = held;
    camera.zoom_at(view(anchor), f32::NAN);
    assert_eq!(camera, held, "a NaN factor is not a zoom");
    camera.zoom_at(view(anchor), 0.0);
    assert_eq!(camera, held, "a zero factor is not a zoom");
    camera.pan((f32::NAN, 0.0));
    assert_eq!(camera, held, "a NaN drag is not a pan");
    println!(
        "zoom ends: ×1 … ×{DEEPEST}, and one frame spends at most {ZOOM_STEP_MAX} of a burst"
    );
}

// --------------------------------------------------------------------------- pan

/// A drag moves the picture by exactly the distance the pointer moved — in screen points,
/// so the same drag is the same picture movement at the world view and at a city block,
/// and the degrees underneath move by that distance divided by the scale.
#[test]
fn a_drag_pans_by_exactly_the_pointer_delta_in_screen_space() {
    let by = vec2(37.5, -21.25);

    for factor in [1.5_f32, 2.45, 12.0, DEEPEST] {
        let mut before = Camera::new(CARD);
        before.zoom_at(Point::new(0.0, 0.0), factor);
        let mut camera = before;
        camera.pan((by.x, by.y));
        // pixels per degree, asked of the projection instead of assumed
        let scale = before.project(Geo::new(1.0, 0.0)).x - before.project(Geo::new(0.0, 0.0)).x;
        assert!(scale > 0.0, "the projection has no scale at ×{factor}");

        for (lon, lat) in PLACES {
            let was = screen(&before, Geo::new(lon, lat));
            let now = screen(&camera, Geo::new(lon, lat));
            // all of the difference is the f32 lattice at 70 000 px — a sixty-fourth of a
            // pixel — and none of it comes from the zoom
            let drifted = (now - was - by).length();
            assert!(
                drifted < 1e-2,
                "at ×{factor} a drag of {by:?} moved ({lon}, {lat}) by {:?} ({drifted} px out)",
                now - was
            );
        }

        // screen points, not degrees: the world under the middle of the plate moves by the
        // drag divided by the zoom, which is what keeps a drag 1:1 at any scale
        let (lon_was, lon_now) = (place(&before, centre()).lon, place(&camera, centre()).lon);
        assert!(
            (lon_now - lon_was + by.x / scale).abs() < 1e-3,
            "at ×{factor} the plate moved {}° for a {} px drag",
            lon_now - lon_was,
            by.x
        );
        let (lat_was, lat_now) = (place(&before, centre()).lat, place(&camera, centre()).lat);
        assert!(
            (lat_now - lat_was - by.y / scale).abs() < 1e-3,
            "at ×{factor} the plate moved {}° for a {} px drag",
            lat_now - lat_was,
            by.y
        );
    }

    // Through the accumulator: one frame of drag is the whole movement since the press,
    // and a drag over several frames sums to the pointer's own travel.
    let mut gesture = Gesture::default();
    let mut at = pointer();
    assert!(by.length() > SLOP, "this drag has to cross the slop: {}", by.length());
    gesture.pointer(true, at);
    at += by;
    gesture.pointer(true, at);
    assert!(gesture.is_panning(), "43 points is a pan");
    let step = gesture.step(1.0 / 60.0);
    assert_eq!(step.pan, by, "a drag is 1:1 with the pointer");
    let mut total = step.pan;
    for _ in 0..4 {
        at += vec2(20.0, 5.0);
        gesture.pointer(true, at);
        total += gesture.step(1.0 / 60.0).pan;
    }
    assert_eq!(
        total,
        by + vec2(80.0, 20.0),
        "a drag accumulates the pointer's travel"
    );

    // A press that wobbles inside the slop moves nothing, and pans the whole distance the
    // moment it crosses — the picture never trails the finger by the slop.
    let mut wobble = Gesture::default();
    wobble.pointer(true, at);
    wobble.pointer(true, at + vec2(2.0, 2.0));
    assert!(vec2(2.0, 2.0).length() < SLOP);
    assert_eq!(wobble.step(1.0 / 60.0).pan, Vec2::ZERO, "a click that wobbles is not a pan");
    assert!(!wobble.is_panning());
    wobble.pointer(true, at + vec2(0.0, -9.0));
    assert_eq!(
        wobble.step(1.0 / 60.0).pan,
        vec2(0.0, -9.0),
        "the pan starts at the press, not at the slop"
    );
}

// -------------------------------------------------------------------- projection

/// Nothing a gesture does may put a place off the projection. Three halves, all of them
/// observable through the public camera: the projection is exact at every zoom, so a
/// coordinate comes back as itself; the clamp keeps the middle of the plate on Earth, so no
/// drag wanders into `lon = 400°`; and the plate is never letterboxed, so no drag shows the
/// air beside the world.
#[test]
fn no_gesture_puts_a_place_off_the_projection() {
    // the projection is its own inverse: a pin can only be drawn off its place by a bug in
    // the camera, never by a pan or a zoom
    for factor in [1.0_f32, 0.7, 1.5, 2.4545, 9.0, 64.0, DEEPEST] {
        let mut camera = Camera::new(CARD);
        camera.zoom_at(Point::new(0.0, 0.0), factor);
        camera.pan((-137.25, 88.5));
        for (lon, lat) in PLACES {
            let point = camera.project(Geo::new(lon, lat));
            assert!(
                point.x.is_finite() && point.y.is_finite(),
                "×{factor}: ({lon}, {lat}) is drawn at {point:?}"
            );
            let back = camera.unproject(point);
            assert!(
                (back.lon - lon).abs() < 1e-2 && (back.lat - lat).abs() < 1e-2,
                "×{factor}: ({lon}, {lat}) came back as ({}, {})",
                back.lon,
                back.lat
            );
        }
    }

    // A long, wild run of pans and zooms, clamped the way every frame clamps them: reaching
    // the deepest zoom and the far corners, so the clamp is exercised in both directions.
    let mut camera = Camera::new(CARD);
    let (mut worst_lon, mut worst_lat) = (0.0_f32, 0.0_f32);
    let (mut deepest, mut shallowest) = (1.0_f32, DEEPEST);
    for round in 0..240_i32 {
        // a hundred thousand pixels either way: at the world view that is nothing, and at
        // the deep end it is further than the plate can go, so the clamp is the thing being
        // measured and not the size of the drag
        let sign = if round % 2 == 0 { 1.0_f32 } else { -1.0 };
        let by = (
            sign * ((round * 337) % 100_000) as f32,
            -sign * ((round * 211) % 100_000) as f32,
        );
        // twenty rounds in, twenty out: the run sweeps both ends of the clamp instead of
        // drifting to one of them
        let factor = if (round / 20) % 2 == 0 { 1.35 } else { 0.74 };
        let anchor = pos2(
            ((round * 131) % 1_000) as f32,
            ((round * 71) % 470) as f32,
        );
        camera.pan(by);
        camera.zoom_at(view(anchor), factor);

        let middle = place(&camera, centre());
        assert!(
            middle.lon.abs() <= 180.0 && middle.lat.abs() <= 90.0,
            "round {round} put the middle of the plate at ({}, {})",
            middle.lon,
            middle.lat
        );
        assert!(
            camera.zoom() >= 1.0 && camera.zoom() <= DEEPEST,
            "round {round}: zoom ×{}",
            camera.zoom()
        );
        // the plate still covers the box: no edge of the world inside it, on either axis
        let (west, east) = (
            camera.project(Geo::new(-180.0, 0.0)).x,
            camera.project(Geo::new(180.0, 0.0)).x,
        );
        let (north, south) = (
            camera.project(Geo::new(0.0, 90.0)).y,
            camera.project(Geo::new(0.0, -90.0)).y,
        );
        assert!(
            west <= -CARD.0 / 2.0 + 0.01
                && east >= CARD.0 / 2.0 - 0.01
                && north <= -CARD.1 / 2.0 + 0.01
                && south >= CARD.1 / 2.0 - 0.01,
            "round {round} letterboxed the plate: x ∈ [{west:.1}, {east:.1}], y ∈ [{north:.1}, {south:.1}]"
        );
        for (place_lon, place_lat) in PLACES {
            let point = camera.project(Geo::new(place_lon, place_lat));
            assert!(
                point.x.is_finite() && point.y.is_finite(),
                "round {round}: ({place_lon}, {place_lat}) at ×{} drew {point:?}",
                camera.zoom()
            );
        }
        worst_lon = worst_lon.max(middle.lon.abs());
        worst_lat = worst_lat.max(middle.lat.abs());
        deepest = deepest.max(camera.zoom());
        shallowest = shallowest.min(camera.zoom());
    }
    assert!(
        deepest > 10.0 && shallowest < 2.0,
        "the run has to reach both ends of the clamp to mean anything: ×{shallowest} … ×{deepest}"
    );
    println!(
        "240 pan/zoom steps: |lon| ≤ {worst_lon:.2}°, |lat| ≤ {worst_lat:.2}°, ×{shallowest:.2} … ×{deepest:.2}"
    );
}

// ------------------------------------------------------------------------ anchor

/// The rule that is easiest to get wrong and hardest to see: a zoom happens around the
/// pointer. What the pointer is on stays under it, and — the discriminating half — the
/// middle of the plate does not, which is the difference between zooming at the reader and
/// zooming the plate.
#[test]
fn a_zoom_is_around_the_pointer_and_not_the_middle_of_the_plate() {
    let anchor_at = pointer();
    let anchor = view(anchor_at);
    assert_ne!(anchor_at, centre(), "the pointer in this test is off centre on purpose");

    // the camera: the world point under the pointer is the same before and after…
    let mut before = Camera::new(CARD);
    before.zoom_at(Point::new(0.0, 0.0), 4.0);
    let mut camera = before;
    camera.zoom_at(anchor, 1.7);
    let was = place(&before, anchor_at);
    let now = place(&camera, anchor_at);
    assert!(
        (was.lon - now.lon).abs() < 1e-3 && (was.lat - now.lat).abs() < 1e-3,
        "the place under the pointer slid: ({}, {}) → ({}, {})",
        was.lon,
        was.lat,
        now.lon,
        now.lat
    );
    // …and the point under the middle of the plate moved, which a plate-centred zoom would
    // not have done
    let middle_was = place(&before, centre());
    let middle_now = place(&camera, centre());
    let slid = (middle_now.lon - middle_was.lon).abs() + (middle_now.lat - middle_was.lat).abs();
    assert!(
        slid > 1.0,
        "a pointer zoom left the middle of the plate where it was: {slid}°"
    );
    // zooming about the middle is a pure scale: there is nothing to compensate, so the
    // place under the middle stays put
    let mut pure = before;
    pure.zoom_at(Point::new(0.0, 0.0), 1.7);
    let pure_middle = place(&pure, centre());
    assert!(
        (pure_middle.lon - middle_was.lon).abs() < 1e-4
            && (pure_middle.lat - middle_was.lat).abs() < 1e-4,
        "a zoom about the middle moved it: ({}, {}) → ({}, {})",
        middle_was.lon,
        middle_was.lat,
        pure_middle.lon,
        pure_middle.lat
    );

    // the accumulator's form of the same rule: `screen = centre + world · k + pan`
    let (pan, k, bounds) = (vec2(-37.0, 51.0), 6.0_f32, (1.0_f32, 400.0_f32));
    let (pan2, k2) = zoom_about(pan, k, 1.7, anchor_at, centre(), bounds);
    let world_at = |pan: Vec2, k: f32, point: Pos2| {
        let offset = point - centre();
        ((offset.x - pan.x) / k, (offset.y - pan.y) / k)
    };
    let (at_was, at_now) = (world_at(pan, k, anchor_at), world_at(pan2, k2, anchor_at));
    assert!(
        (at_was.0 - at_now.0).abs() < 1e-3 && (at_was.1 - at_now.1).abs() < 1e-3,
        "the anchor moved: {at_was:?} → {at_now:?}"
    );
    let (middle_was, middle_now) = (
        world_at(pan, k, centre()),
        world_at(pan2, k2, centre()),
    );
    assert!(
        (middle_now.0 - middle_was.0).abs() + (middle_now.1 - middle_was.1).abs() > 1.0,
        "the middle of the plate did not move: {middle_was:?} → {middle_now:?}"
    );

    // end to end through the plate's motion: a wheel zoom at the pointer, eased over the
    // frames it takes, and the place under the pointer is still there when it lands
    let mut motion = Motion::new(before);
    motion.zoom_by(0.4, anchor);
    let mut eased = before;
    let mut frames = 0;
    while frames < 240 && (motion.is_moving() || !settled(&eased, &motion.target)) {
        eased = motion.step(eased, 1.0 / 60.0).0;
        frames += 1;
    }
    assert!(settled(&eased, &motion.target), "the ease must land, in {frames} frames");
    assert!(
        eased.zoom() > before.zoom(),
        "and it must have zoomed in: ×{} → ×{}",
        before.zoom(),
        eased.zoom()
    );
    let landed = place(&motion.target, anchor_at);
    assert!(
        (was.lon - landed.lon).abs() < 1e-3 && (was.lat - landed.lat).abs() < 1e-3,
        "the target puts the pointer on ({}, {}) instead of ({}, {})",
        landed.lon,
        landed.lat,
        was.lon,
        was.lat
    );
    let kept = place(&eased, anchor_at);
    assert!(
        (was.lon - kept.lon).abs() < 0.5 && (was.lat - kept.lat).abs() < 0.5,
        "the place under the pointer slid during the ease: ({}, {}) → ({}, {})",
        was.lon,
        was.lat,
        kept.lon,
        kept.lat
    );
    println!(
        "×1.7 at the pointer keeps ({:.2}°, {:.2}°) and moves the plate's middle {slid:.2}°; \
         eased, ×{:.2} → ×{:.2} in {frames} frames",
        was.lon,
        was.lat,
        before.zoom(),
        eased.zoom()
    );
}

// --------------------------------------------------------------------------- click

/// Three places: one dot with nine stories, one a tenth of a pixel away (`0.02°` of
/// longitude at the opening zoom of 2.78 px per degree), so the plate draws **one** dot for
/// the pair, and one alone in the Pacific.
///
/// The pair is the point: a click has to answer a pin's index, not a dot's, and the index it
/// answers is the strongest pin behind the dot — the one whose story the page will open.
fn pins() -> Vec<Pin> {
    vec![
        Pin { place: "Kinshasa".to_string(), lat: -4.3, lon: 15.3, count: 9, article: 11 },
        Pin { place: "Brazzaville".to_string(), lat: -4.27, lon: 15.28, count: 1, article: 12 },
        Pin { place: "Lima".to_string(), lat: -12.0, lon: -77.0, count: 3, article: 13 },
    ]
}

/// One frame with these events, drawn by `draw`, with the texture deltas released.
///
/// `epaint 0.37` panics on `Drop` when a `TexturesDelta` still carries unapplied deltas
/// (`epaint-0.37.0/src/textures.rs:337`); a real backend uploads them, there is none here,
/// and the assertions below read what `show` answered, which needs no texture at all.
fn frame(
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    mut draw: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(plate()),
            events,
            ..Default::default()
        },
        |ui| draw(ui),
    );
    output.textures_delta.clear();
    output
}

/// The events of a press or a release at `at`. The move comes first, so the plate under the
/// point is the one that takes the press.
fn pointer_events(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// What the user asked for: a click on a pin answers **that pin's index**, so the page above
/// the plate can open `pins[index].article` — and a click on the water answers nothing, so a
/// miss never opens the wrong story.
#[test]
fn a_click_on_a_pin_answers_that_pins_index() {
    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);
    // no outlines: this test is about the dots, and a missing file is an empty world, not a
    // crash (`views::map::World::load`)
    let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
    let pins = pins();

    // the box the plate gets, read rather than assumed: everything below is measured from it
    let mut box_ = Rect::NOTHING;
    let mut answered = None;
    frame(&ctx, Vec::new(), |ui| {
        box_ = ui.available_rect_before_wrap();
        answered = view.show(ui, &pins, CARD.1);
    });
    assert!(
        answered.is_none(),
        "a frame with no pointer answered {answered:?}, but only a click may answer a pin"
    );
    assert!(
        box_.width() <= CARD.0 + 1.0 && box_.height() <= CARD.1 + 1.0,
        "the headless card is {:?}, larger than {CARD:?}",
        box_.size()
    );
    // The view seats the world in the plate's own 2:1 box inside the card, so the plate — not
    // the card — is what a position on the map is measured from, and the camera is the one the
    // frame fitted to it rather than a second one built here.
    let plate = view.plate_rect().expect("the first frame drew no plate");
    assert!(
        (plate.center() - box_.center()).length() < 0.5 && (plate.width() / plate.height() - 2.0).abs() < 0.01,
        "the plate is {plate:?} in the card {box_:?}: not the world's 2:1 box at its middle"
    );

    // the camera the first frame framed, and the plate it painted it in
    let camera = view.camera();
    let to_screen = |camera: &Camera, at: Geo| -> Pos2 {
        let point = camera.project(at);
        pos2(plate.center().x + point.x, plate.center().y + point.y)
    };
    let click = |at: Pos2, view: &mut MapView| -> Option<usize> {
        frame(&ctx, pointer_events(at, true), |ui| {
            view.show(ui, &pins, CARD.1);
        });
        let mut answered = None;
        frame(&ctx, pointer_events(at, false), |ui| {
            answered = view.show(ui, &pins, CARD.1);
        });
        answered
    };

    // the strong pin of the pair: one dot, drawn for both, answered as the pin that carries
    // the story
    let kinshasa = to_screen(&camera, Geo::new(15.3, -4.3));
    let answered = click(kinshasa, &mut view);
    let index = answered.unwrap_or_else(|| {
        panic!(
            "a click on the dot at {kinshasa:?} answered nothing; the dots were {} places",
            pins.len()
        )
    });
    assert_eq!(
        pins[index].article, 11,
        "the click answered pin {index} ({:?}), not the story filed at Kinshasa",
        pins[index].place
    );

    // the pin on its own, at the far side of the plate
    let lima = to_screen(&camera, Geo::new(-77.0, -12.0));
    let answered = click(lima, &mut view);
    let index = answered.unwrap_or_else(|| panic!("a click on Lima at {lima:?} answered nothing"));
    assert_eq!(
        pins[index].article, 13,
        "the click answered pin {index} ({:?}), not the story filed at Lima",
        pins[index].place
    );

    // and the water: a click that misses every dot answers nothing at all
    let water = pos2(plate.right() - 40.0, plate.bottom() - 40.0);
    assert!(
        click(water, &mut view).is_none(),
        "a click at {water:?}, 400 px from the nearest pin, answered a story"
    );
}
