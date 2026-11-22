//! The accumulator as an inertial surface, pinned from outside the library.
//!
//! `views/map.rs` and `views/graph.rs` both spend their gestures through
//! `atlas_desktop::gesture`, and the complaint that started this was about a trackpad:
//! a pinch arrives as `zoom_delta`, a two-finger scroll as a wheel delta, and macOS
//! delivers both in bursts of uneven size. Four properties make that feel like a
//! surface instead of a switch, and each has a test here:
//!
//! * a sub-pixel delta is not dropped — the small frames of a slow pinch are most of it;
//! * one large delta is one event's worth, never a jump (the clamp is per event);
//! * a coast decays by elapsed time, so 30 Hz and 120 Hz travel the same;
//! * a gesture belongs to the pointer position where it started, and half a second of
//!   silence is a new gesture.

use std::time::Duration;

use egui::{pos2, Pos2, Vec2};

use atlas_desktop::gesture::{
    zoom_about, Gesture, Step, PAN_EVENT_MAX, PINCH_EVENT_MAX, WHEEL_EVENT_MAX, WHEEL_GAIN,
};

/// The Overview's map plate: 1 000 x 470 points. A camera fitted to it has
/// `k = 1000 / 360` = 2.78 points per degree, the whole world in the plate's width.
const PLATE: Vec2 = Vec2::new(1000.0, 470.0);

/// The pointer, a little up and to the left of the middle, so a zoom pivoting on it and
/// a zoom pivoting on the plate's centre are different numbers.
fn pointer() -> Pos2 {
    pos2(380.0, 190.0)
}

/// Step until the accumulator is at rest, summing the zoom it applied in log space.
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
        assert!(frames < 5_000, "a gesture must be spent, not kept: {log} so far");
    }
}

/// A real trackpad stream, in points of ctrl-scroll per event, at 60 events per second.
///
/// The shape is macOS's, not a sine wave: the fingers take up the strain with two or
/// three points, close through the middle of the pinch at four to nine points a frame,
/// and stop with a point and a half as they come to rest — and every size recurs
/// unevenly, because the OS coalesces whatever arrived into the frame it has. 120 events
/// is two seconds of that, carrying `SUM` of zoom in total.
const STREAM: [f32; 24] = [
    1.2, 2.8, 6.4, 3.1, 8.7, 2.2, 7.5, 4.4, 9.2, 0.4, 5.8, 3.6, //
    7.1, 1.6, 8.9, 2.4, 6.2, 4.8, 9.6, 0.8, 5.1, 3.9, 7.8, 1.4,
];

/// The 120-event stream: `STREAM` five times over.
fn stream() -> Vec<f32> {
    (0..120).map(|index| STREAM[index % STREAM.len()]).collect()
}

/// What the events carry between them, in log space, clamped the way one event is.
fn carried(events: &[f32]) -> f32 {
    events
        .iter()
        .map(|points| (points * WHEEL_GAIN).clamp(-WHEEL_EVENT_MAX, WHEEL_EVENT_MAX))
        .sum()
}

/// A camera to spend the zoom on: `screen = centre + world * k + pan`, like the plate's.
struct Camera {
    pan: Vec2,
    k: f32,
}

impl Camera {
    fn new() -> Camera {
        Camera { pan: Vec2::ZERO, k: PLATE.x / 360.0 }
    }

    /// What a world point under `anchor` looks like: the invariant is that it does not
    /// move while the zoom is spent.
    fn under(&self, anchor: Pos2, centre: Pos2) -> Vec2 {
        (anchor - centre - self.pan) / self.k
    }

    fn apply(&mut self, step: Step, centre: Pos2) {
        let (pan, k) = zoom_about(
            self.pan,
            self.k,
            step.zoom,
            step.anchor,
            centre,
            (PLATE.x / 360.0, 400.0),
        );
        self.pan = pan;
        self.k = k;
    }
}

fn centre() -> Pos2 {
    pos2(PLATE.x / 2.0, PLATE.y / 2.0)
}

#[test]
fn a_trackpad_stream_zooms_one_way_and_lands_on_its_own_total() {
    // Delivered at 60 events a second into a 60 Hz frame loop: two events per frame, in
    // the uneven sizes above. Nothing about the delivery — one event or eight per frame,
    // large or small — may change where the gesture ends.
    let events = stream();
    let mut gesture = Gesture::default();
    let mut camera = Camera::new();
    let anchor = pointer();
    let mut spent = 0.0;
    let mut frames = 0;
    for pair in events.chunks(2) {
        for points in pair {
            gesture.zoom(*points, 1.0, anchor);
        }
        let step = gesture.step(1.0 / 60.0);
        spent += step.zoom.ln();
        frames += 1;
        // monotone: every frame zooms in, and a camera that is zooming in never zooms out
        assert!(step.zoom >= 1.0, "frame {frames} reversed the zoom: x{}", step.zoom);
        let before = camera.k;
        let under = camera.under(anchor, centre());
        camera.apply(step, centre());
        assert!(camera.k >= before, "frame {frames} un-zoomed the camera: {before} → {}", camera.k);
        // and the place under the pointer stays under it, frame by frame
        let moved = (camera.under(anchor, centre()) - under).length();
        assert!(moved < 1e-4, "frame {frames} slid the place under the pointer by {moved}");
    }
    // "within a pixel": the total is the sum of the events. 1e-3 of a log unit moves a
    // point 500 points away from the anchor by half a pixel, so this is tighter than one.
    let wanted = carried(&events);
    let lost = (spent - wanted).abs();
    assert!(
        lost < 1e-3,
        "the stream carried {wanted} log but spent {spent}: {lost} lost over {frames} frames"
    );
    // and it is a zoom worth having: this uneven stream is about 2.4x
    assert!(spent > 0.8, "two seconds of pinching applied only {spent} log");
    assert!(camera.k > 2.0 * PLATE.x / 360.0, "the plate ended at k = {}", camera.k);
}

#[test]
fn a_pinch_made_of_sub_pixel_frames_is_not_dropped() {
    // A gentle pinch is delivered as frames of a tenth of a point: egui turns one point
    // of ctrl-scroll into e^(1/200), so each frame is a factor of 1.0005. Two seconds of
    // that is a 6% zoom, and all of it has to arrive — a threshold of feel (the old 1e-3)
    // dropped every frame.
    let gentle = (0.1_f32 / 200.0).exp();
    assert!((gentle - 1.0).abs() < 1e-3, "the fixture is sub-threshold: {gentle}");
    let mut gesture = Gesture::default();
    for _ in 0..120 {
        gesture.zoom(0.0, gentle, pointer());
    }
    let (spent, _) = spend(&mut gesture, 1.0 / 60.0);
    let wanted = 120.0 * gentle.ln();
    assert!(
        (spent - wanted).abs() < 1e-4,
        "a 6% pinch spent {spent} of {wanted} log"
    );
    // and a frame's worth of it is applied, not parked: one event, one frame
    let mut one = Gesture::default();
    one.zoom(0.0, gentle, pointer());
    let step = one.step(1.0 / 60.0);
    assert!(step.zoom > 1.0, "a sub-pixel pinch frame was dropped");
    assert!((step.zoom.ln() - gentle.ln()).abs() < 1e-7);

    // The same pinch, delivered into more frames, is the same pinch — and the old
    // threshold made that depend on the panel. A 1.5x pinch over two seconds is 3.4e-3 of
    // a factor per frame at 60 Hz and 1.7e-3 at 120 Hz, both over 1e-3; the same pinch
    // over four seconds at 120 Hz is 480 frames of 8.5e-4 each, and every one of those was
    // dropped, so a slow pinch arrived either in full or not at all depending on how fast
    // the window happened to be drawing.
    let delivered = |frames: usize| -> f32 {
        let factor = 1.5_f32.powf(1.0 / frames as f32);
        let mut gesture = Gesture::default();
        for _ in 0..frames {
            gesture.zoom(0.0, factor, pointer());
        }
        spend(&mut gesture, 1.0 / 60.0).0
    };
    let wanted = 1.5_f32.ln();
    for frames in [120, 240, 480, 960] {
        let got = delivered(frames);
        assert!(
            (got - wanted).abs() < 1e-3,
            "the same pinch delivered {got} in {frames} frames, of {wanted}"
        );
    }
}

#[test]
fn one_large_delta_is_one_event_s_worth_and_no_jump() {
    // A page-unit wheel event and an impossible pinch: each is clamped where it enters,
    // so no single event can cross the plate, and a burst of them cannot spend faster
    // than the largest single event.
    let mut page = Gesture::default();
    page.zoom(1_000_000.0, 1.0, pointer());
    let (log, _) = spend(&mut page, 1.0 / 60.0);
    assert!((log - WHEEL_EVENT_MAX).abs() < 1e-5, "a page scroll asked for {log}");
    let mut huge = Gesture::default();
    huge.zoom(0.0, 1e6, pointer());
    let (log, _) = spend(&mut huge, 1.0 / 60.0);
    assert!((log - PINCH_EVENT_MAX).abs() < 1e-5, "an impossible pinch asked for {log}");
    // one frame of the largest event is at most that event, never the whole burst
    let mut burst = Gesture::default();
    for _ in 0..20 {
        burst.zoom(1_000_000.0, 1.0, pointer());
    }
    let first = burst.step(1.0 / 60.0);
    assert!(
        first.zoom.ln() <= WHEEL_EVENT_MAX + 1e-5,
        "a frame of the burst applied {} log, more than one event's worth",
        first.zoom.ln()
    );

    // A warp of the pointer is the same rule for the pan: one sample moves the plate
    // `PAN_EVENT_MAX` points, not five thousand, and the coast does not inherit the
    // warp's speed.
    let mut drag = Gesture::default();
    drag.pointer(true, pos2(100.0, 100.0));
    drag.pointer(true, pos2(5_100.0, 100.0));
    let step = drag.step(1.0 / 30.0);
    assert_eq!(step.pan.x, PAN_EVENT_MAX, "a warped sample moved the plate {}", step.pan.x);
    drag.pointer(false, pos2(5_100.0, 100.0));
    let mut coasted = 0.0;
    for _ in 0..600 {
        let step = drag.step(1.0 / 30.0);
        coasted += step.pan.x;
        if !step.running {
            break;
        }
    }
    assert!(coasted <= 4_000.0 / 5.5 + 1.0, "the warp flung the plate {coasted} points");
}

/// Drive the 120-event stream (60 events a second, two seconds) with a frame loop at
/// `fps`, and answer the log zoom applied, the camera's final `k`, and the frames drawn.
fn run(fps: f32) -> (f32, f32, usize) {
    let events = stream();
    let mut gesture = Gesture::default();
    let mut camera = Camera::new();
    let anchor = pointer();
    let per_frame = 60.0 / fps;
    let (mut taken, mut log, mut frames) = (0usize, 0.0, 0usize);
    loop {
        frames += 1;
        let due = ((frames as f32) * per_frame).floor() as usize;
        while taken < due.min(events.len()) {
            gesture.zoom(events[taken], 1.0, anchor);
            taken += 1;
        }
        let step = gesture.step(1.0 / fps);
        log += step.zoom.ln();
        camera.apply(step, centre());
        if taken >= events.len() && !step.running {
            return (log, camera.k, frames);
        }
        assert!(frames < 10_000, "the stream did not settle in {frames} frames");
    }
}

#[test]
fn the_same_stream_lands_on_the_same_zoom_at_thirty_and_at_a_hundred_and_twenty_hertz() {
    // The same two seconds of pinching, sampled into frames at 30 Hz and at 120 Hz: two
    // events per frame at one, one every other frame at the other. Frames shape the
    // transit; they may not shape where the gesture ends.
    let (slow_log, slow_k, slow_frames) = run(30.0);
    let (fast_log, fast_k, fast_frames) = run(120.0);
    let wanted = carried(&stream());
    assert!(
        (slow_log - wanted).abs() < 1e-3 && (fast_log - wanted).abs() < 1e-3,
        "30 Hz spent {slow_log} and 120 Hz {fast_log}, of {wanted}"
    );
    assert!(
        (slow_log - fast_log).abs() < 1e-3,
        "the same stream landed on {slow_log} at 30 Hz and {fast_log} at 120 Hz"
    );
    assert!(
        (slow_k - fast_k).abs() < slow_k * 1e-3,
        "the plate ended at k = {slow_k} at 30 Hz and {fast_k} at 120 Hz"
    );
    // and the frame counts are nothing alike, or the test would be measuring nothing
    assert!(
        fast_frames > slow_frames * 2,
        "{slow_frames} frames at 30 Hz against {fast_frames} at 120 Hz"
    );
}

#[test]
fn a_coast_travels_by_elapsed_time_not_by_frame_count() {
    // One second of identical hand movement, released, then half a second of coast at
    // three refresh rates: the distance is the same, because the decay is integrated
    // over `dt` instead of multiplied in per frame.
    let travel = |fps: f32| -> f32 {
        let dt = 1.0 / fps;
        let mut gesture = Gesture::default();
        let mut at = pos2(100.0, 100.0);
        let frames = fps.round() as usize;
        for _ in 0..frames {
            at.x += 2_400.0 * dt;
            gesture.pointer(true, at);
            gesture.step(dt);
        }
        gesture.pointer(false, at);
        let mut moved = 0.0;
        for _ in 0..(fps * 0.5).round() as usize + 40 {
            let step = gesture.step(dt);
            moved += step.pan.x;
            if !step.running {
                break;
            }
        }
        moved
    };
    let (slow, middle, fast) = (travel(30.0), travel(60.0), travel(120.0));
    assert!(slow > 100.0, "the coast is real: {slow}");
    for (rate, distance) in [(30.0, slow), (60.0, middle), (120.0, fast)] {
        assert!(
            (distance - slow).abs() < slow * 0.02,
            "at {rate} Hz the coast was {distance}, at 30 Hz {slow}"
        );
    }
}

#[test]
fn a_half_second_of_silence_starts_a_new_gesture_at_the_new_pointer() {
    // A gesture is one intention and it is anchored where it started. Ten events of a
    // burst arriving between two frames, with the pointer wandering across the plate,
    // are all spent around the first one.
    let first = pos2(200.0, 150.0);
    let mut burst = Gesture::default();
    burst.zoom(6.0, 1.0, first);
    burst.zoom(6.0, 1.0, pos2(260.0, 150.0));
    burst.zoom(6.0, 1.0, pos2(320.0, 150.0));
    assert_eq!(burst.step(1.0 / 60.0).anchor, first, "the burst drifted to another anchor");

    // A pause of half a second is not part of that gesture: the reader stopped pinching
    // and started again, somewhere else, so the new zoom pivots where the pointer is now.
    let elsewhere = pos2(700.0, 320.0);
    std::thread::sleep(Duration::from_millis(520));
    burst.zoom(6.0, 1.0, elsewhere);
    assert_eq!(
        burst.step(1.0 / 60.0).anchor,
        elsewhere,
        "half a second of silence kept the old anchor"
    );

    // and two events with no gap between them are still one gesture, even after that
    burst.zoom(6.0, 1.0, pos2(720.0, 320.0));
    assert_eq!(burst.step(1.0 / 60.0).anchor, elsewhere);
}
