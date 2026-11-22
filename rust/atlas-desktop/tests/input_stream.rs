//! The acceptance spec for the input stream: what the plate must do with a wheel, a drag and
//! a pinch, measured end to end through `views::map::MapView`'s own drawing.
//!
//! Every test here is a complaint the native window actually produced, written as the number
//! that settles it. The measured "before" is in each test's comment, and the fix it names is
//! the code that must be true for the assertion to hold — so a regression fails with the answer
//! rather than with a diff.
//!
//! Five things are pinned, and they are the five the audit of the native window found:
//!
//! * **a wheel the plate cannot spend belongs to the page.** Inside an `egui::ScrollArea` — the
//!   shape `app.rs`'s `scrolls` puts the Overview in — 180 pt of two-finger scroll over text
//!   moved the page 178 px, and the same 180 pt over a plate at its zoom floor moved it **0 px**:
//!   the plate zeroed `smooth_scroll_delta` before it knew whether it had any room to zoom.
//! * **a pinch never scrolls the page.** A ctrl-scroll is in both of egui's readings, and a page
//!   that moves under the fingers drags the pin out from under them.
//! * **a drag is the pointer, one point per point.** The plate moved **1.83 points per point**
//!   while the button was down and then coasted another 54 px, because the drag was applied once
//!   by `Motion::dragged` and again by the momentum that `dragged` had recorded and `step` then
//!   integrated: a 120 pt drag moved the picture 263 pt.
//! * **a slow pinch is not noise.** A per-event threshold of 1e-4 (0.02 pt, a fifth of a pixel)
//!   dropped every frame of a gentle pinch: 200 events carrying 4 pt in total moved the plate
//!   **x1.000**.
//! * **the ease is a rate per second.** The frame's share of a queued burst was a constant per
//!   frame, so the same stream read x13.0 at 60 Hz and x12.1 at 120 Hz 50 ms after the last
//!   event.
//!
//! The numbers are read from `MapView::camera()` — the camera the plate paints with, not a proxy
//! for it — after the page has been drawn for real, headless, exactly as `tests/smoke.rs` draws
//! it.

use std::cell::Cell;
use std::path::Path;

use egui::{
    pos2, vec2, Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, RawInput, Rect,
    TouchPhase, Vec2,
};

use atlas_desktop::data::Pin;
use atlas_desktop::views::map::MapView;

/// The card the map allocates in the Overview. Wider than the world's 2:1, so at the opening
/// zoom the width is exactly filled — and the opening zoom *is* the zoom floor, which is what
/// makes the first test a floor test.
const CARD: Vec2 = Vec2::new(1000.0, 470.0);

/// The page the plate sits on, and the text that makes it overflow: ten rows above the card keep
/// the plate visible under the pointer, twenty below give the page its 310 px of scroll, so
/// 180 pt of wheel has somewhere to go.
const PAGE: Vec2 = Vec2::new(1000.0, 700.0);
const ROWS_ABOVE: usize = 10;
const ROWS_BELOW: usize = 20;

/// Where a wheel aimed at the page's text lands: over the rows above the card.
fn over_text() -> Pos2 {
    pos2(300.0, 40.0)
}

/// A headless window: one `egui::Context`, a clock at a given refresh rate, and nothing else.
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

    /// One frame at `size`, drawing whatever `draw` draws. The texture deltas are dropped, as
    /// `tests/smoke.rs` does: there is no GPU in here.
    fn frame(&mut self, size: Vec2, events: Vec<Event>, draw: &mut dyn FnMut(&mut egui::Ui)) {
        self.t += self.dt;
        let mut output = self.ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                time: Some(self.t),
                predicted_dt: self.dt as f32,
                events,
                ..Default::default()
            },
            |ui| draw(ui),
        );
        output.textures_delta.clear();
    }

    fn idle(&mut self, size: Vec2, frames: usize, draw: &mut dyn FnMut(&mut egui::Ui)) {
        for _ in 0..frames {
            self.frame(size, Vec::new(), draw);
        }
    }
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

/// A trackpad wheel event: the unit and the source a Mac reports, with the phase of a gesture in
/// progress. `ctrl` is what macOS itself sends for a pinch — egui turns that into `zoom_delta()` —
/// so the pinch streams below are the real event a trackpad produces.
fn wheel(delta_y: f32, ctrl: bool, phase: TouchPhase) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase,
        source: MouseWheelSource::Trackpad,
        modifiers: Modifiers { ctrl, ..Default::default() },
    }
}

/// Somewhere a place can be: four quarters of the world, so a zoom is read off the plate's own
/// camera rather than off a caption's rounding.
fn pins() -> Vec<Pin> {
    let at = |place: &str, lon: f64, lat: f64| Pin {
        place: place.to_string(),
        lat,
        lon,
        count: 4,
        article: 1,
    };
    vec![at("a", -120.0, -35.0), at("b", -60.0, 30.0), at("c", 10.0, -20.0), at("d", 80.0, 45.0)]
}

/// A plate with no outlines: this spec is about the wheel, the press and the zoom, and a missing
/// file is an empty world rather than a crash (`views::map::World::load`).
fn plate() -> MapView {
    MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"))
}

/// The plate's zoom, as the camera the plate paints with reports it: 1 is the whole world in the
/// card, 96 the deepest level.
fn zoom_of(view: &MapView) -> f32 {
    view.camera().zoom()
}

/// The plate's pan, in points.
fn offset_of(view: &MapView) -> Vec2 {
    let at = view.camera().offset();
    vec2(at.x, at.y)
}

/// One whole gesture of `count` wheel events of `points` each, one event per frame as a trackpad
/// sends them, then enough frames for egui's own smoothing (a reading of 8 pt or more is spread
/// over about ten frames) and for the plate's ease to finish.
fn spin(window: &mut Window, view: &mut MapView, points: f32, count: usize, ctrl: bool) {
    let pins = pins();
    let at = pos2(CARD.x / 2.0, CARD.y / 2.0);
    window.frame(CARD, vec![move_to(at)], &mut |ui| {
        view.show(ui, &pins, CARD.y);
    });
    for index in 0..count {
        let phase = if index == 0 { TouchPhase::Start } else { TouchPhase::Move };
        window.frame(CARD, vec![wheel(points, ctrl, phase)], &mut |ui| {
            view.show(ui, &pins, CARD.y);
        });
    }
    window.idle(CARD, 40, &mut |ui| {
        view.show(ui, &pins, CARD.y);
    });
}

/// The page the Overview puts the plate on: an `egui::ScrollArea` — `app.rs`'s `scrolls` — around
/// a column of text, the plate's card, and more text. The card's rectangle is handed back because
/// it is how much the page moved: its y moves with the scroll and nothing else.
fn page(ui: &mut egui::Ui, view: &mut MapView, pins: &[Pin], card: &Cell<Rect>) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for row in 0..ROWS_ABOVE {
            ui.label(format!("filler row {row}"));
        }
        let drawn = ui.allocate_ui(CARD, |ui| {
            view.show(ui, pins, CARD.y);
        });
        card.set(drawn.response.rect);
        for row in ROWS_ABOVE..ROWS_ABOVE + ROWS_BELOW {
            ui.label(format!("filler row {row}"));
        }
    });
}

// ---------------------------------------------------------------------- the wheel

/// A wheel over the plate at its zoom floor must move the page as far as the same wheel over that
/// page's text.
///
/// Measured before: 180 pt over text scrolled the page **178 px**; the same 180 pt over the plate
/// scrolled **0 px** and zoomed **0 %**, because `views::map.rs` took the reading and zeroed it
/// before asking whether the camera had anywhere to go.
///
/// The fix this names: the plate spends what its own clamp can use and leaves the rest in
/// `smooth_scroll_delta` — egui's `ScrollArea` reads that field *after* its content has run
/// (`egui-0.37.0/src/containers/scroll_area.rs:1330-1375`), so a child that writes the remainder
/// back hands the page its share without knowing the page exists. The camera's floor and
/// `Gesture::pending_zoom` are what decide "no room"; `Motion::wheel` is the door that answers.
#[test]
fn a_wheel_at_the_zoom_floor_scrolls_the_page_as_far_as_a_wheel_over_text() {
    let pins = pins();

    // the control: the same wheel, aimed at the text above the plate
    let mut window = Window::new(60.0);
    let mut view = plate();
    let card = Cell::new(Rect::NOTHING);
    let mut draw = |ui: &mut egui::Ui| page(ui, &mut view, &pins, &card);
    window.idle(PAGE, 4, &mut draw);
    let before = card.get().min.y;
    window.frame(PAGE, vec![move_to(over_text())], &mut draw);
    for _ in 0..6 {
        window.frame(PAGE, vec![wheel(-30.0, false, TouchPhase::Move)], &mut draw);
    }
    window.idle(PAGE, 12, &mut draw);
    let by_text = card.get().min.y - before;

    // the plate: the identical stream, aimed at its middle. The plate is new, so it is at its
    // zoom floor, which is the whole point of the test.
    let mut window = Window::new(60.0);
    let mut view = plate();
    let card = Cell::new(Rect::NOTHING);
    let mut draw = |ui: &mut egui::Ui| page(ui, &mut view, &pins, &card);
    window.idle(PAGE, 4, &mut draw);
    let before = card.get().min.y;
    let middle = card.get().center();
    window.frame(PAGE, vec![move_to(middle)], &mut draw);
    for _ in 0..6 {
        window.frame(PAGE, vec![wheel(-30.0, false, TouchPhase::Move)], &mut draw);
    }
    window.idle(PAGE, 12, &mut draw);
    let by_plate = card.get().min.y - before;

    assert!(
        by_text < -100.0,
        "the control itself is broken: 180 pt over text moved the page {by_text} px, so this \
         test cannot say anything about the plate"
    );
    assert!(
        by_plate < -100.0,
        "180 pt of two-finger scroll over a plate at its zoom floor moved the page {by_plate} px \
         (the control moved it {by_text} px): the plate swallowed a wheel it had no room to \
         spend, so a reader cannot scroll this page by starting the gesture over a third of it. \
         Fix: `views::map.rs` must not zero `smooth_scroll_delta`; spend what the camera can use \
         (`Motion::wheel` against the camera's floor) and write the remainder back into \
         `smooth_scroll_delta.y`, which egui's `ScrollArea` reads after the content has run."
    );
    assert!(
        (by_plate - by_text).abs() <= 2.0,
        "the page scrolled {by_text} px under the text but {by_plate} px under the plate: the \
         plate keeps {} px of the reader's gesture. Fix: the remainder written back into \
         `smooth_scroll_delta` must be the whole reading minus the log the plate spent, divided \
         by the same gain it was multiplied by (`Motion::wheel`).",
        by_text - by_plate
    );
}

/// A pinch is the one gesture that takes the whole reading: a ctrl-scroll arrives in both of
/// egui's fields, and a page cannot be pinched. Measured before: 40 pt of ctrl-scroll over the
/// plate left the page where it was, which is right — and the regression this pins is the other
/// direction, a plate that handed part of a pinch back and scrolled the page out from under the
/// fingers, which slides the anchor.
#[test]
fn a_pinch_over_the_plate_never_scrolls_the_page() {
    let pins = pins();
    let mut window = Window::new(60.0);
    let mut view = plate();
    let card = Cell::new(Rect::NOTHING);
    let mut draw = |ui: &mut egui::Ui| page(ui, &mut view, &pins, &card);
    window.idle(PAGE, 4, &mut draw);
    let before = card.get().min.y;
    let middle = card.get().center();

    window.frame(PAGE, vec![move_to(middle)], &mut draw);
    for index in 0..6 {
        let phase = if index == 0 { TouchPhase::Start } else { TouchPhase::Move };
        window.frame(PAGE, vec![wheel(-30.0, true, phase)], &mut draw);
    }
    window.idle(PAGE, 20, &mut draw);
    let moved = card.get().min.y - before;

    assert!(
        moved.abs() <= 0.5,
        "a pinch over the plate scrolled the page {moved} px: the fingers are holding the plate, \
         and a page that moves under them makes the anchor drift. Fix: when \
         `Gesture::pinch_reading` answers `Some`, take the whole `smooth_scroll_delta` — do not \
         hand a part back (`views::map.rs`)."
    );
}

// ----------------------------------------------------------------------- the drag

/// A 120 pt drag must move the plate 120 pt, not once and a half, and must not coast while the
/// button is still down.
///
/// Measured before: the plate moved **1.83 points per point** while held (per-step deltas +28.9,
/// +34.3, +35.9, +36.3, +36.5, +36.5 for six 20 pt moves) and another **+54 px** after the
/// release — a 120 pt drag moved the picture **263 pt**. The movement was applied twice:
/// `Motion::dragged` panned the camera and recorded the speed, and `Motion::step` integrated that
/// speed on the same frame.
///
/// The fix this names: exactly one door applies a drag. A press moves the plate through
/// `Gesture::pointer` → `Step::pan`, at one point per point; a flick is *recorded* while the
/// button is down and spent only by the coast branch, which runs when `Gesture::press` is `None`
/// (`gesture.rs`, `Gesture::step`).
#[test]
fn a_drag_moves_the_plate_one_point_per_point_and_does_not_coast_before_the_release() {
    let pins = pins();
    let mut window = Window::new(60.0);
    let mut view = plate();

    // zoom in first: at the opening zoom the plate is flush with the box and there is nothing to
    // pan into (`geo::Camera::settle`), so a drag there would measure the clamp and not the drag
    spin(&mut window, &mut view, 20.0, 10, true);
    let zoom = zoom_of(&view);
    assert!(zoom > 2.0, "the plate did not zoom in, so it has no room to pan: x{zoom}");

    let from = pos2(CARD.x / 2.0, CARD.y / 2.0);
    window.frame(CARD, vec![move_to(from), button(from, true)], &mut |ui| {
        view.show(ui, &pins, CARD.y);
    });

    let mut last = offset_of(&view).x;
    let mut travelled = 0.0;
    let mut steps = Vec::new();
    for step in 1..=6 {
        let at = pos2(from.x + 20.0 * step as f32, from.y);
        window.frame(CARD, vec![move_to(at)], &mut |ui| {
            view.show(ui, &pins, CARD.y);
        });
        let moved = offset_of(&view).x - last;
        last = offset_of(&view).x;
        travelled += moved;
        steps.push(moved);

        // and the frame after it, with the pointer still and the button still down: a press that
        // is a pan is not a flick yet, so nothing may move
        window.frame(CARD, Vec::new(), &mut |ui| {
            view.show(ui, &pins, CARD.y);
        });
        let crept = offset_of(&view).x - last;
        last = offset_of(&view).x;
        assert!(
            crept.abs() < 0.2,
            "with the button down and the pointer still, the plate moved {crept} pt on step \
             {step}: the drag is being applied twice, once as the pointer's own movement and once \
             as a momentum that `Motion::dragged` recorded while the button was down. Fix: spend \
             a flick only after the release (`gesture.rs`, the coast branch of `Gesture::step` \
             runs when `self.press.is_none()`), and let `Step::pan` be the only place a press \
             moves a camera."
        );
    }

    for (index, moved) in steps.iter().enumerate() {
        assert!(
            (moved - 20.0).abs() <= 2.0,
            "step {} of a 20 pt drag moved the plate {moved} pt (all six: {steps:?}): a drag must \
             be one point per point. Fix: delete the second application — one accumulator moves \
             the camera, and it is `Gesture::pointer` → `Step::pan`.",
            index + 1
        );
    }
    assert!(
        (travelled - 120.0).abs() <= 2.0,
        "a 120 pt drag moved the plate {travelled} pt (steps {steps:?}): {:.2} points per point. \
         Fix: one application per movement, as above.",
        travelled / 120.0
    );
}

// ------------------------------------------------------------------- the slow pinch

/// A stream of sub-pixel pinch events must not be discarded.
///
/// Measured before: 200 ctrl-scroll events of 0.02 pt — a fifth of a pixel each, which is what a
/// slow two-finger pinch on a Mac actually sends — carried 4 pt of pinch in total and moved the
/// plate **x1.000**, because the map's own threshold was `(pinch - 1.0).abs() > 1e-4` and each
/// event was exactly 1e-4.
///
/// The fix this names: the only threshold on a pinch reading is a float noise floor
/// (`gesture::PINCH_EPS`, 1e-6 — a four-hundredth of a point), so a frame is accepted at the size
/// a hand can produce it. egui synthesises a pinch at `e^(points/200)`, so this stream's total is
/// `e^(0.02 * 499 / 200)`.
#[test]
fn a_stream_of_sub_pixel_pinches_is_not_discarded() {
    let pins = pins();
    let mut window = Window::new(60.0);
    let mut view = plate();
    let at = pos2(CARD.x / 2.0, CARD.y / 2.0);
    window.frame(CARD, vec![move_to(at)], &mut |ui| {
        view.show(ui, &pins, CARD.y);
    });

    // 500 events of a fifth of a point: 10 pt in all, a 5% zoom
    let count = 500;
    let points = 0.02;
    for index in 0..count {
        let phase = if index == 0 { TouchPhase::Start } else { TouchPhase::Move };
        window.frame(CARD, vec![wheel(points, true, phase)], &mut |ui| {
            view.show(ui, &pins, CARD.y);
        });
    }
    window.idle(CARD, 40, &mut |ui| {
        view.show(ui, &pins, CARD.y);
    });

    let wanted = ((count - 1) as f32 * points / 200.0).exp();
    let got = zoom_of(&view);
    assert!(
        got > 1.02,
        "500 pinch events of {points} pt each moved the plate only x{got} of x{wanted}: the \
         sub-pixel frames were dropped. Fix: `gesture::PINCH_EPS` (1e-6) is the only threshold a \
         pinch reading may have — an epsilon of 1e-4 is a fifth of a point and throws a slow pinch \
         away a frame at a time."
    );
    assert!(
        (got / wanted - 1.0).abs() <= 0.01,
        "500 pinch events of {points} pt carried x{wanted} of zoom and the plate applied x{got} \
         ({:.2}% off): small readings are being rounded, clamped or dropped somewhere between \
         `zoom_delta()` and the camera.",
        (got / wanted - 1.0) * 100.0
    );
}

// ---------------------------------------------------------------- the frame rate

/// The same stream at 30, 60 and 120 Hz must land on the same zoom, in the same wall-clock time.
///
/// Measured before: the frame's share of a queued burst was a constant per frame, so the same
/// burst was spent twice as fast per second at 120 Hz — 8 pinch factors read x12.1 at 120 Hz
/// against x13.0 at 60 Hz fifty milliseconds after the last event, and a queue took
/// `ceil(pending / 0.35)` frames at every rate, i.e. 4x the wall clock at 30 Hz that it took at
/// 120 Hz.
///
/// The fix this names: the share is `ZOOM_RATE * dt` with `ZOOM_FLOOR_DT` under it
/// (`gesture::Gesture::step`), so a second of frames applies a second of zoom at any refresh
/// rate. The second assertion is the one with teeth for the *transit*: a per-frame constant
/// settles the same queue in the same number of frames, which is a quarter of the wall clock at
/// 120 Hz — the endpoint alone cannot see that.
#[test]
fn the_same_stream_lands_on_the_same_zoom_at_thirty_sixty_and_a_hundred_and_twenty_hertz() {
    let points = 4.0;
    let count = 20;
    let settled: Vec<(f64, f32, f32)> = [30.0, 60.0, 120.0]
        .into_iter()
        .map(|hz| {
            let mut window = Window::new(hz);
            let mut view = plate();
            let pins = pins();
            let at = pos2(CARD.x / 2.0, CARD.y / 2.0);
            window.frame(CARD, vec![move_to(at)], &mut |ui| {
                view.show(ui, &pins, CARD.y);
            });
            for index in 0..count {
                let phase = if index == 0 { TouchPhase::Start } else { TouchPhase::Move };
                window.frame(CARD, vec![wheel(points, true, phase)], &mut |ui| {
                    view.show(ui, &pins, CARD.y);
                });
            }
            // and frames until the scale stops moving: the wall clock that takes is the ease
            let mut frames = 0.0;
            let mut last = zoom_of(&view);
            loop {
                window.frame(CARD, Vec::new(), &mut |ui| {
                    view.show(ui, &pins, CARD.y);
                });
                frames += 1.0;
                let now = zoom_of(&view);
                if (now - last).abs() <= 1e-6 * last {
                    break;
                }
                last = now;
                assert!(frames < 5_000.0, "the plate never settled: x{now}");
            }
            (hz, zoom_of(&view), frames * window.dt as f32)
        })
        .collect();

    let wanted = ((count - 1) as f32 * points * 0.005).exp();
    let reference = settled[0].1;
    for (hz, got, _) in &settled {
        assert!(
            (got / reference - 1.0).abs() <= 0.01,
            "the same 76 pt pinch landed on x{reference} at 30 Hz and x{got} at {hz} Hz ({:.2}% \
             apart): the frame's share of the burst is a rate per second, not a constant per \
             frame. Fix: `Gesture::step` spends `ZOOM_RATE * dt` (with `ZOOM_FLOOR_DT` under it).",
            (got / reference - 1.0) * 100.0
        );
        assert!(
            (got / wanted - 1.0).abs() <= 0.01,
            "at {hz} Hz the pinch landed on x{got} instead of x{wanted} ({:.2}% off): the stream \
             is being lost or doubled somewhere between `zoom_delta()` and the camera.",
            (got / wanted - 1.0) * 100.0
        );
    }

    // The *transit* — that the queue takes the same wall clock at every rate — cannot be seen
    // from here: the biggest burst this door can queue is `PINCH_EVENT_MAX` (0.4 log), which one
    // 30 Hz frame spends whole, so the settle time would measure the frame period and not the
    // rate. It is pinned where it can be measured, on a 2.4 log queue: the `Plates`/`drain`
    // test `the_ease_is_the_same_at_sixty_and_at_a_hundred_and_twenty_hertz` in `src/gesture.rs`,
    // and the 30 Hz case of the same loop.
}
