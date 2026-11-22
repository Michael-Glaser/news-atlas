//! Is the plate's wheel and pinch path *continuous*? Measured, not asserted from taste.
//!
//! Five properties, each a number this file prints and then checks:
//!
//! 1. **No step beyond the contract.** One event may queue at most [`PINCH_EVENT_MAX`] of log
//!    zoom, and one frame may spend at most `ZOOM_RATE * dt` of it — so the largest factor a
//!    single frame can put on the screen is `e^(13 * dt)`, `1.24x` at 60 Hz, whatever arrived. A
//!    burst lands as a slide, never as a jump, and no frame of a zoom-in ever goes backwards.
//! 2. **The ends hold.** Under a storm of wheel events the camera never leaves `[1, 96]` (the fill
//!    and `MAX_ZOOM`, read off `geo`), and each end is actually reached rather than approached.
//! 3. **A hand-back at the floor does not jitter.** At the floor the plate has nothing to spend
//!    and gives the whole reading to the page. The camera must not move by a float noise floor's
//!    worth per frame — a shimmer at the plate's edge — and the page must move one way only.
//! 4. **A drag is the pointer.** One point of pointer travel must be one point of picture wherever
//!    the clamp allows movement, measured as the camera's own offset rather than a proxy.
//! 5. **A burst costs one frame.** 200 wheel events in one frame do the work of one reading: the
//!    frame's cost is the same as a single event's (minimum of [`SAMPLES`] timings) and it applies
//!    the share one reading of that size is allowed.
//!
//! Timings are the minimum of [`SAMPLES`] runs, because a scheduling hiccup can only add time: the
//! minimum is the closest estimate of what the code costs.
//!
//! Everything is drawn headless through `egui::Context::run_ui`, as `tests/smoke.rs` does, so what
//! is measured is the shipped drawing and input code with no window and no GPU.

use std::cell::Cell;
use std::path::Path;
use std::time::Instant;

use egui::{
    pos2, vec2, Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, RawInput, Rect,
    TouchPhase, Vec2,
};

use atlas_desktop::data::Pin;
use atlas_desktop::geo::{Camera, Point};
use atlas_desktop::gesture::{PINCH_EVENT_MAX, SLOP, ZOOM_RATE};
use atlas_desktop::views::map::{MapView, Motion};

/// The card the map allocates in the Overview, and the box every camera below is fitted to.
const CARD: Vec2 = Vec2::new(1000.0, 470.0);

/// A 60 Hz frame, which is the clock the contract is stated at.
const HZ: f64 = 1.0 / 0.016_666_667;

/// How many timed runs a timing is the minimum of.
const SAMPLES: usize = 15;

/// The deepest zoom `geo` will hold, in multiples of the fit. The constant behind it is private to
/// `geo` on purpose, so it is written here once and the end is *reached* rather than assumed.
const DEEPEST: f32 = 96.0;

/// The tolerance a drawn position is compared at, in points.
const PIXEL: f32 = 0.05;

fn card() -> Rect {
    Rect::from_min_size(Pos2::ZERO, CARD)
}

fn centre() -> Pos2 {
    card().center()
}

fn move_to(at: Pos2) -> Event {
    Event::PointerMoved(at)
}

fn button(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::default(),
    }
}

/// A trackpad wheel reading. `ctrl` is what macOS sends for a pinch, which egui turns into
/// `zoom_delta()`; the plain wheel is the two-finger scroll.
fn wheel(delta_y: f32, ctrl: bool, phase: TouchPhase) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase,
        source: MouseWheelSource::Trackpad,
        modifiers: Modifiers { ctrl, ..Default::default() },
    }
}

/// A plate and the page it sits on, so a frame can draw either without a closure holding the view.
///
/// A `&mut MapView` captured by a drawing closure is also a view nothing else may read, and every
/// assertion below is about the camera — so the harness takes the plate by argument instead.
struct Plate {
    view: MapView,
    pins: Vec<Pin>,
    /// The card's rectangle on screen, kept for the page test where it moves.
    card: Cell<Rect>,
    /// Draw the Overview's shape around it: a scrolling column of text with the card inside.
    paged: bool,
}

impl Plate {
    /// A plate with no outlines: this file is about the camera, and a missing file is an empty
    /// world rather than a crash (`views::map::World::load`).
    fn new(paged: bool) -> Plate {
        let at = |place: &str, lon: f64, lat: f64| Pin {
            place: place.to_string(),
            lat,
            lon,
            count: 4,
            article: 1,
        };
        Plate {
            view: MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson")),
            pins: vec![
                at("a", -120.0, -35.0),
                at("b", -60.0, 30.0),
                at("c", 10.0, -20.0),
                at("d", 80.0, 45.0),
            ],
            card: Cell::new(Rect::NOTHING),
            paged,
        }
    }

    fn camera(&self) -> Camera {
        self.view.camera()
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        if !self.paged {
            self.view.show(ui, &self.pins, CARD.y);
            self.card.set(Rect::from_min_size(Pos2::ZERO, CARD));
            return;
        }
        let (view, pins, card) = (&mut self.view, &self.pins, &self.card);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for row in 0..12 {
                ui.label(format!("row {row}"));
            }
            let drawn = ui.allocate_ui(CARD, |ui| {
                view.show(ui, pins, CARD.y);
            });
            card.set(drawn.response.rect);
            for row in 12..34 {
                ui.label(format!("row {row}"));
            }
        });
    }
}

/// A headless window: one context, one clock, one plate.
struct Window {
    ctx: egui::Context,
    t: f64,
    dt: f64,
}

impl Window {
    fn new(hz: f64) -> Window {
        let ctx = egui::Context::default();
        atlas_desktop::theme::install(&ctx);
        Window { ctx, t: 0.0, dt: 1.0 / hz }
    }

    /// One frame, timed. The texture deltas are dropped, as `tests/smoke.rs` does.
    fn timed(&mut self, size: Vec2, events: Vec<Event>, plate: &mut Plate) -> f64 {
        self.t += self.dt;
        let dt = self.dt as f32;
        let started = Instant::now();
        let Window { ctx, t, .. } = self;
        let mut output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                time: Some(*t),
                predicted_dt: dt,
                events,
                ..Default::default()
            },
            |ui| plate.draw(ui),
        );
        let spent = started.elapsed().as_secs_f64();
        output.textures_delta.clear();
        spent
    }

    fn frame(&mut self, size: Vec2, events: Vec<Event>, plate: &mut Plate) {
        self.timed(size, events, plate);
    }

    fn idle(&mut self, size: Vec2, frames: usize, plate: &mut Plate) {
        for _ in 0..frames {
            self.frame(size, Vec::new(), plate);
        }
    }
}

// ------------------------------------------------------- 1. no step beyond the contract

/// One event may queue at most [`PINCH_EVENT_MAX`] of log zoom, one frame may spend at most
/// `ZOOM_RATE * dt`, and a zoom-in never steps backwards.
///
/// The per-frame bound is the one a hand feels: `e^(13/60)` = 1.242, so no event of any size can
/// put more than a quarter of a zoom on the screen in one frame at 60 Hz. The per-event bound is
/// what keeps one impossible number from crossing the plate on its own. Both are measured for a
/// ladder of magnitudes, including ones far past anything a device sends.
#[test]
fn no_zoom_step_exceeds_the_contract_per_event() {
    let dt = 1.0 / HZ as f32;
    let frame_ceiling = (ZOOM_RATE * dt).exp();
    println!(
        "contract: one event <= e^{PINCH_EVENT_MAX} = {:.4}x, one frame <= e^{:.4} = \
         {frame_ceiling:.4}x",
        PINCH_EVENT_MAX.exp(),
        ZOOM_RATE * dt
    );
    let mut worst_frame: f32 = 1.0;
    let mut worst_event: f32 = 1.0;
    for points in [1.0_f32, 5.0, 40.0, 170.0, 1_000.0, 100_000.0] {
        let mut motion = Motion::new(Camera::new((CARD.x, CARD.y)));
        let mut camera = Camera::new((CARD.x, CARD.y));
        motion.wheel(points, Point::new(60.0, -40.0));
        let mut spent = 0.0;
        let mut frames = 0;
        let mut largest: f32 = 1.0;
        loop {
            let (next, running) = motion.step(camera, dt);
            let step = next.zoom() / camera.zoom();
            camera = next;
            spent += step.ln();
            frames += 1;
            largest = largest.max(step);
            assert!(
                step <= frame_ceiling * 1.0001,
                "{points} pt in one event made a single frame zoom {step:.4}x, past the contract \
                 {frame_ceiling:.4}x = e^(ZOOM_RATE {ZOOM_RATE} * dt {dt:.5}). Fix: a frame's share \
                 is `ZOOM_RATE * dt` and nothing may exceed it (`gesture::Gesture::step`)."
            );
            assert!(
                step >= 1.0 - 1e-6,
                "{points} pt: a frame of a zoom-in stepped backwards to {step:.6}x — the queue is \
                 over-spending and unwinding (`gesture::Gesture::step`)."
            );
            if !running || frames > 500 {
                break;
            }
        }
        println!(
            "  {points:>10} pt: one event queued {spent:.4} log = {:.4}x over {frames} frames, \
             largest frame {largest:.4}x",
            spent.exp()
        );
        worst_frame = worst_frame.max(largest);
        worst_event = worst_event.max(spent.exp());
        assert!(
            spent <= PINCH_EVENT_MAX + 1e-4,
            "one {points} pt event carried {spent:.4} log = {:.4}x, past the per-event contract \
             {PINCH_EVENT_MAX} = {:.4}x. Fix: an event is clamped once, at the door \
             (`Motion::wheel` → `PINCH_EVENT_MAX`, `Gesture::zoom_log`).",
            spent.exp(),
            PINCH_EVENT_MAX.exp()
        );
    }
    println!(
        "worst: one event {worst_event:.4}x, one frame {worst_frame:.4}x of a {frame_ceiling:.4}x \
         contract"
    );
}

// ---------------------------------------------------------------- 2. the ends hold

/// Under 400 wheel events each way the camera never leaves `[1, 96]` and reaches each end.
///
/// A zoom that leaks past the floor shows as a plate that shrinks inside its card — air beside the
/// world; one that leaks past the ceiling asks `geo` for outlines it does not have. Both are
/// measured per frame here, not only at the end.
#[test]
fn the_zoom_floor_and_ceiling_hold_under_a_storm() {
    let dt = 1.0 / HZ as f32;
    let mut motion = Motion::new(Camera::new((CARD.x, CARD.y)));
    let mut camera = Camera::new((CARD.x, CARD.y));
    let anchor = Point::new(0.0, 0.0);
    let mut lowest: f32 = f32::MAX;
    let mut highest: f32 = 0.0;
    let mut to_ceiling = None;
    let mut to_floor = None;

    for (direction, points) in [(1.0_f32, 170.0_f32), (-1.0, -170.0)] {
        for frame in 0..400 {
            motion.wheel(points, anchor);
            let (next, _) = motion.step(camera, dt);
            camera = next;
            let zoom = camera.zoom();
            lowest = lowest.min(zoom);
            highest = highest.max(zoom);
            assert!(
                (1.0 - 1e-4..=DEEPEST + 1e-3).contains(&zoom),
                "frame {frame} of {direction:+.0} x 170 pt left the camera at x{zoom}, outside the \
                 ends [1, {DEEPEST}]. Fix: `geo::Camera::settle` clamps the scale, and no view may \
                 write the scale around it."
            );
            if direction > 0.0 && to_ceiling.is_none() && zoom >= DEEPEST * 0.999 {
                to_ceiling = Some(frame + 1);
            }
            if direction < 0.0 && to_floor.is_none() && zoom <= 1.0 + 1e-4 {
                to_floor = Some(frame + 1);
            }
        }
        println!(
            "  after 400 x {points:+.0} pt: x{:.4} (range so far {lowest:.4}..{highest:.4})",
            camera.zoom()
        );
    }
    println!(
        "ceiling reached in {to_ceiling:?} frames, floor in {to_floor:?}; the whole storm stayed \
         in {lowest:.4}..{highest:.4}"
    );
    assert!(
        (camera.zoom() - 1.0).abs() < 1e-3,
        "400 events out ended at x{} instead of the floor 1.0",
        camera.zoom()
    );
    assert!(to_ceiling.is_some(), "400 events in never reached x{DEEPEST}");
    assert!(to_floor.is_some(), "coming back, the floor was never reached");
    assert!(
        highest <= DEEPEST + 1e-3,
        "the deepest a single frame reached was x{highest}, past MAX_ZOOM x{DEEPEST}"
    );
    assert!(lowest >= 1.0 - 1e-4, "a frame dipped to x{lowest}, below the fill");
}

// ------------------------------------------- 3. a hand-back at the floor does not jitter

/// At the floor the plate gives the whole reading to the page — and must not move itself.
///
/// Before the hand-back existed the plate ate 180 pt of two-finger scroll and the page moved 0 px.
/// A jitter here is the opposite failure: a plate that takes a float noise floor's worth of each
/// reading shimmers at its edge while the page scrolls under it, and a page that moves back and
/// forth is worse than one that does not move.
#[test]
fn a_hand_back_at_the_floor_does_not_jitter() {
    let page = vec2(1000.0, 700.0);
    let mut window = Window::new(HZ);
    let mut plate = Plate::new(true);
    window.idle(page, 6, &mut plate);
    let start = plate.card.get().min.y;
    let at = plate.card.get().center();
    window.frame(page, vec![move_to(at)], &mut plate);

    let mut positions = Vec::new();
    let mut zoom_drift: f32 = 0.0;
    let mut offset_drift: f32 = 0.0;
    for _ in 0..60 {
        window.frame(page, vec![wheel(-30.0, false, TouchPhase::Move)], &mut plate);
        positions.push(plate.card.get().min.y - start);
        zoom_drift = zoom_drift.max((plate.camera().zoom() - 1.0).abs());
        let offset = plate.camera().offset();
        offset_drift = offset_drift.max(offset.x.abs().max(offset.y.abs()));
    }
    let mut reversals = 0;
    let mut largest: f32 = 0.0;
    let mut previous = 0.0;
    for position in &positions {
        largest = largest.max((position - previous).abs());
        if *position > previous + PIXEL {
            reversals += 1;
        }
        previous = *position;
    }
    let travelled = positions.last().copied().unwrap_or(0.0);
    println!(
        "60 x -30 pt at the floor: page moved {travelled:.1} px, largest frame {largest:.1} px, \
         reversals {reversals}, camera drift {zoom_drift:.2e} zoom / {offset_drift:.2e} pt"
    );
    assert!(
        zoom_drift < 1e-6,
        "the plate's zoom drifted {zoom_drift:.2e} off the floor while handing every reading \
         back: it is spending a part of a reading it has no room for (`Motion::room`)."
    );
    assert!(
        offset_drift < 0.01,
        "the plate's centre drifted {offset_drift:.2e} pt while the page scrolled under the \
         pointer: the hand-back is moving the camera it was measured from."
    );
    assert_eq!(
        reversals, 0,
        "the page reversed {reversals} times in 60 notches (positions {positions:?}): the \
         hand-back has to be the same reading every frame, or the page fights the plate."
    );
    assert!(
        travelled < -100.0,
        "60 wheel notches at the floor moved the page {travelled:.1} px: the reading was swallowed \
         (`views/map.rs` must write `Motion::wheel`'s answer back into `smooth_scroll_delta.y`)."
    );
}

// -------------------------------------------------------------- 4. a drag is the pointer

/// One point of pointer travel is one point of picture wherever the clamp allows movement.
///
/// Two things are measured rather than excused. The fit: with the plate flush with its box
/// (`geo::Camera`'s slack is zero there) a drag must move nothing, because anything else would show
/// air beside the world. And the first [`SLOP`] points of a press: they are still a click, so the
/// picture does not move — but once the press crosses the slop it takes the *whole* distance since
/// the press, so the picture never trails the finger by the slop. Each case prints the zoom it
/// reached and the slack it had.
#[test]
fn a_drag_is_one_point_per_point_at_every_zoom() {
    let steps = [1.0_f32, 7.0, 20.0, 120.0];
    let wanted: f32 = steps.iter().sum();
    for target in [1.0_f32, 1.5, 10.0, DEEPEST] {
        let mut window = Window::new(HZ);
        let mut plate = Plate::new(false);
        window.idle(CARD, 3, &mut plate);
        // reach the zoom through the plate's own door: one small ctrl-scroll per frame, settled
        let at = centre();
        window.frame(CARD, vec![move_to(at)], &mut plate);
        let mut frames = 0;
        while plate.camera().zoom() < target * 0.999 && frames < 900 {
            window.frame(CARD, vec![wheel(5.0, true, TouchPhase::Move)], &mut plate);
            frames += 1;
        }
        window.idle(CARD, 40, &mut plate);
        let zoom = plate.camera().zoom();
        let slack = slack_x(plate.camera());

        // a press at the middle, then moves of 1, 7, 20 and 120 points to the right
        let from = centre();
        window.frame(CARD, vec![move_to(from), button(from, true)], &mut plate);
        let mut worst: f32 = 0.0;
        let mut travelled = 0.0;
        let mut pointer = from;
        let mut log = Vec::new();
        for step in steps {
            let before = plate.camera().offset().x;
            pointer = pos2(pointer.x + step, pointer.y);
            window.frame(CARD, vec![move_to(pointer)], &mut plate);
            let by = plate.camera().offset().x - before;
            travelled += by;
            // the picture is at the pointer's whole distance since the press, unless the press has
            // not crossed the slop yet (still a click) or the clamp leaves no room at all
            let since_press = pointer.x - from.x;
            let expected = if slack < wanted {
                0.0
            } else if since_press <= SLOP {
                0.0
            } else {
                since_press
            };
            log.push((step, by, expected - travelled + by));
            worst = worst.max((by - (expected - (travelled - by))).abs());
        }
        window.frame(CARD, vec![button(pointer, false)], &mut plate);
        println!(
            "  zoom x{zoom:.4} (slack {slack:.2} pt): pointer {wanted:.0} pt → picture \
             {travelled:.2} pt, worst step error {worst:.3} pt (move, picture, lag: {log:?})"
        );
        assert!(
            worst <= PIXEL,
            "at x{zoom:.4} with {slack:.2} pt of slack the picture did not follow the pointer: \
             worst error {worst:.3} pt over 1/7/20/120 pt moves. Fix: one application per movement \
             — `Gesture::pointer` → `Step::pan` → `Camera::pan` — and no second pan by \
             `drag_delta()` in the view."
        );
        if slack >= wanted {
            assert!(
                (travelled - wanted).abs() <= PIXEL,
                "at x{zoom:.4} a {wanted:.0} pt drag moved the picture {travelled:.2} pt"
            );
        } else {
            assert!(
                travelled.abs() <= slack + PIXEL,
                "at x{zoom:.4} the plate had {slack:.2} pt of slack and a {wanted:.0} pt drag \
                 moved it {travelled:.2} pt past it"
            );
        }
    }
}

/// How far the camera may still be panned sideways, in points, before it shows air beside the
/// world: `geo` clamps the centre to half the overhang, and a horizontal drag is bounded by the
/// wider of the plate's two overhangs.
fn slack_x(camera: Camera) -> f32 {
    let fill = (CARD.x / 360.0).max(CARD.y / 180.0);
    let k = fill * camera.zoom();
    let edge = (360.0 * k - CARD.x) / 2.0;
    (edge - camera.offset().x.abs()).max(0.0)
}

// -------------------------------------------------------------- 5. a burst is one frame

/// 200 wheel events in one frame cost one frame's work and one reading's zoom step.
///
/// The complaint that started this is a trackpad handing egui ten events between two frames; 200
/// is twenty times that, and the frame must neither take twenty times as long nor jump twenty times
/// as far. Cost is the minimum of [`SAMPLES`] runs, so a scheduling hiccup in one of them cannot be
/// read as the code being slow.
#[test]
fn a_burst_of_two_hundred_events_costs_one_frame() {
    let frame_ceiling = (ZOOM_RATE / HZ as f32).exp();
    let mut single = f64::MAX;
    let mut burst = f64::MAX;
    let mut single_step: f32 = 0.0;
    let mut burst_step: f32 = 0.0;

    for _ in 0..SAMPLES {
        for events in [1_usize, 200] {
            let mut window = Window::new(HZ);
            let mut plate = Plate::new(false);
            window.idle(CARD, 2, &mut plate);
            let at = centre();
            window.frame(CARD, vec![move_to(at)], &mut plate);
            let before = plate.camera().zoom();
            let mut stream: Vec<Event> =
                (0..events).map(|_| wheel(5.0, false, TouchPhase::Move)).collect();
            stream.insert(0, move_to(at));
            let spent = window.timed(CARD, stream, &mut plate);
            let step = plate.camera().zoom() / before;
            if events == 1 {
                single = single.min(spent);
                single_step = single_step.max(step);
            } else {
                burst = burst.min(spent);
                burst_step = burst_step.max(step);
            }
        }
    }
    let ratio = burst / single.max(f64::MIN_POSITIVE);
    println!(
        "min of {SAMPLES}: 1 event {:.0} us, 200 events {:.0} us (x{ratio:.2}); that frame's zoom \
         {single_step:.4}x vs {burst_step:.4}x of a {frame_ceiling:.4}x contract",
        single * 1e6,
        burst * 1e6
    );
    assert!(
        ratio < 4.0,
        "a frame carrying 200 wheel events cost {ratio:.2}x a frame carrying one ({:.0} us vs \
         {:.0} us, min of {SAMPLES}): per-event work is being repeated per frame. Fix: read one \
         wheel value per frame (`ui.input_mut` → `Motion::wheel`) and let the accumulator carry \
         the rest — no view may loop over `RawInput::events`.",
        burst * 1e6,
        single * 1e6
    );
    assert!(
        burst_step <= frame_ceiling * 1.0001,
        "200 events in one frame zoomed {burst_step:.4}x in that frame, past the contract \
         {frame_ceiling:.4}x: the burst is applied at the event, not spent by the frame."
    );

    // and the whole burst settles like one reading, inside the ease rather than in one jump
    let mut window = Window::new(HZ);
    let mut plate = Plate::new(false);
    window.idle(CARD, 2, &mut plate);
    let at = centre();
    window.frame(CARD, vec![move_to(at)], &mut plate);
    let start = plate.camera().zoom();
    let mut stream: Vec<Event> = (0..200).map(|_| wheel(5.0, false, TouchPhase::Move)).collect();
    stream.insert(0, move_to(at));
    window.frame(CARD, stream, &mut plate);
    let mut frames = 1;
    let mut last = plate.camera().zoom();
    loop {
        window.frame(CARD, Vec::new(), &mut plate);
        frames += 1;
        let now = plate.camera().zoom();
        if (now - last).abs() <= 1e-7 * now {
            break;
        }
        last = now;
        assert!(frames < 100, "the burst never settled: {frames} frames, zoom {now:.4}");
    }
    let total = plate.camera().zoom() / start;
    println!("the same 200-event burst landed as {total:.4}x over {frames} frames");
    assert!(
        frames <= 20,
        "350 points of wheel took {frames} frames to land ({total:.4}x): the burst is being \
         re-spent every frame"
    );
}
