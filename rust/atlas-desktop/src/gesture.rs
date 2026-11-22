//! One gesture accumulator for every canvas in the window.
//!
//! The map and the graph hold the same camera (`screen = centre + world * k + pan`) and each
//! had its own arithmetic for it. Every complaint the native window produced about zooming
//! came out of that arithmetic being in two places, and out of it being done per event:
//!
//! * a zoom was applied *where the event arrived*. A trackpad hands egui ten wheel events
//!   between two frames, so the plate added ten logarithms and the next frame applied
//!   `exp(0.9) ≈ 2.5x` in one jump — the "zooming is not smooth" report;
//! * nothing decayed by *time*: a constant per frame zooms twice as far per second on a
//!   120 Hz panel as on a 60 Hz one, and a release stopped dead instead of coasting;
//! * a drag was applied twice — once as the pointer's own movement and once as the momentum
//!   the same filter had recorded and then integrated — so a 120 pt drag moved the plate
//!   **263 pt**;
//! * a per-event threshold of a tenth of a percent threw a slow pinch away a frame at a time:
//!   200 ctrl-scroll events of 0.02 pt carried 4 pt of pinch and moved the plate nothing;
//! * a wheel the camera could not spend was swallowed, so two fingers over a plate at its
//!   floor scrolled the page **0 px** where the same gesture over that page's text scrolled it
//!   **178 px**.
//!
//! So the rules here, and they are the whole module:
//!
//! * an event never moves a camera. It adds to a *pending* zoom (in log space, where
//!   gestures are additive) or to a pending pan, and [`Gesture::step`] hands one frame
//!   one bounded share of each;
//! * nothing a device reports is thrown away for being small. A pinch frame carrying a
//!   tenth of a point is a factor of 1.0005 (egui synthesises it at `e^(points * 1/200)`,
//!   `egui-0.37.0/src/input_state/mod.rs:113`), so the thresholds here are float noise
//!   floors, not thresholds of feel: a slow pinch whose small frames are dropped loses its
//!   whole zoom;
//! * every bound is per event, never per frame: one event may ask for at most
//!   [`WHEEL_EVENT_MAX`] or [`PINCH_EVENT_MAX`] of zoom and [`PAN_EVENT_MAX`] of pan, so
//!   a coalesced burst adds up to its honest total while a single impossible delta
//!   cannot cross the plate. The frame's share is a *rate*: it shapes the transit, not
//!   the total;
//! * the share is proportional to `dt`, so the same burst takes the same wall-clock
//!   time at 60 Hz and at 120 Hz — the ease is a rate per second, not per frame;
//! * momentum is the exact integral of its own exponential decay over `dt`, so a
//!   flick travels the same distance at any refresh rate, and it ends below a floor
//!   instead of crawling forever;
//! * a gesture is one intention: its zoom is anchored at the pointer where the gesture
//!   *started*, and [`GESTURE_GAP`] of silence is what starts the next one;
//! * a press is not a pan until the pointer has travelled [`SLOP`] points, so a click
//!   that wobbles one pixel still opens what it was aimed at;
//! * everything is clamped: one event, the pending total, one frame's share, the
//!   velocity a flick may leave with, and the frame time itself.
//!
//! What it is *not*: it owns no camera and clamps no world. Only a view knows how far
//! its world may be dragged, so the pan comes back to the view to clamp, and the zoom
//! clamp is passed to [`zoom_about`] as a `(min, max)` of `k`.
//!
//! ## How a view spends it, once per frame
//!
//! `views::map` is the caller, and this is the order it uses: one read of the frame's input,
//! then one accumulator, then one step applied to the view's own camera.
//!
//! ```ignore
//! let (scroll, pinch, dt) = ui.input(|i| {
//!     (i.smooth_scroll_delta.y, i.zoom_delta(), i.stable_dt)
//! });
//! let at = ui.input(|i| i.pointer.latest_pos()).unwrap_or_else(|| response.rect.center());
//!
//! // A pinch is the one gesture that takes the whole reading: egui synthesises a ctrl-scroll
//! // into `zoom_factor_delta` (`input_state/mod.rs:462-466`) *and* leaves it in
//! // `smooth_scroll_delta`, and a page cannot be pinched. A wheel takes only the part the
//! // view's own clamp can use, and the view writes the rest back.
//! if Gesture::pinch_reading(pinch).is_some() {
//!     gesture.zoom(0.0, pinch, at);
//!     ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
//! } else if scroll != 0.0 {
//!     let left = view.wheel(scroll, at);              // the view's clamp decides
//!     ui.input_mut(|i| i.smooth_scroll_delta.y = left);
//! }
//!
//! // The press, every frame, so a release is never missed. This is what pans a canvas: a
//! // press that crossed `SLOP` moves the plate through `Step::pan`, one point per point.
//! gesture.pointer(response.is_pointer_button_down_on() || response.dragged(), at);
//!
//! let step = gesture.step(dt);
//! camera.pan((step.pan.x, step.pan.y));
//! camera.zoom_at(step.anchor, step.zoom);
//! if step.running {
//!     ui.ctx().request_repaint();                     // a coast or a burst is still going
//! }
//! ```
//!
//! Two things a view must **not** do, because the accumulator already did them: pan the camera
//! by `response.drag_delta()` as well (that is `Step::pan`), and integrate a velocity of its
//! own (`Step::pan` carries the coast). A click is still a click while the press is inside the
//! slop: `response.clicked() && !gesture.is_panning()`.
//!
//! Writing the leftover wheel back into `smooth_scroll_delta` is what lets a plate at its floor
//! scroll the page it sits on: egui's `ScrollArea` reads that field *after* its content has run
//! (`egui-0.37.0/src/containers/scroll_area.rs:1330-1375`), so a child that writes the
//! remainder back hands the page its share without knowing the page exists.
//!
//! Two platform notes a caller has to live with: a `TouchPhase::Start` wheel event has its
//! delta dropped by egui (`input_state/wheel_state.rs:127-131`), so the first frame of every
//! Mac gesture carries nothing; and a Point delta of 8 or more is spread over about ten frames
//! (`wheel_state.rs:171-205`), so a fast wheel starts ~150 ms late. Neither is ours to fix, and
//! neither loses anything.
//!
//! ## What a gesture is guaranteed to do when the fingers stop
//!
//! A queued burst is spent at [`ZOOM_RATE`] per second, so it settles in
//! `ceil(ZOOM_PENDING_MAX / (ZOOM_RATE * dt))` frames — 12 at 60 Hz, 23 at 120 Hz —
//! plus the one frame that finds nothing left, and then `Step::running` is false,
//! `Step::zoom` is exactly `1.0`, and the scale does not move again. Two opposite
//! bursts of the same size cancel to the log's own rounding, so a pinch in and back
//! out returns to the scale it started from; a burst the *clamp* refused is spent
//! there and never kept, so it cannot unwind later under the fingers.
//!
//! `tests/input_stream.rs` is the acceptance spec for the five complaints above, and
//! `tests/map_zoom_pan.rs` and `tests/gesture_smoothing.rs` are the arithmetic's own.

use std::time::Instant;

use egui::{Pos2, Vec2};

/// Log-zoom per scrolled point of a plain wheel handed to [`Gesture::zoom`]. One notch of a
/// Logitech wheel is 14 points (`egui-0.37.0/src/input_state/wheel_state.rs:165`), so a notch
/// is `e^0.021` — 1.02x, a percent of a zoom — and ten points is the 1.5% the map's own test
/// measures.
///
/// A view with a world of its own keeps its own gain and calls [`Gesture::zoom_log`] instead:
/// a plate of coastlines wants a steeper notch than a graph of nodes. `views::map`'s is
/// `0.0022`, and the doc on it says what that measurement was.
pub const WHEEL_GAIN: f32 = 0.0015;

/// The most zoom one scroll event may ask for, in log units (1.25x). A smooth trackpad
/// sends under 8 points per event (`wheel_state.rs:171-183`), i.e. 0.012 here; a wheel
/// in Line or Page units sends hundreds, and this is what stops a page-scroll from
/// being one jump. At `WHEEL_EVENT_MAX` and this gain, the event is 147 points.
pub const WHEEL_EVENT_MAX: f32 = 0.22;

/// The most zoom one pinch event may ask for, in log units (1.5x). `zoom_delta()` is a
/// ratio of fingertip distances, and a fast pinch frame is about 1.05 (0.05), so this
/// is eight times a real event's worth: it exists for a device that reports a pinch as
/// one impossible factor, not to shape a gesture.
pub const PINCH_EVENT_MAX: f32 = 0.4;

/// The zoom is never trusted beyond this much pending, in log units (11x either way):
/// a stuck device can queue an urgent zoom, not an unbounded one. At `ZOOM_RATE` this
/// is 185 ms of spending — long enough for a real pinch, which never gets near it.
pub const ZOOM_PENDING_MAX: f32 = 2.4;

/// What one second of frames may apply, in log units. The fastest real pinch closes a
/// factor of two in about 300 ms — 2.3 log/s — so this spends what a hand produces
/// eight times over and a gesture never banks. The frame's share is this rate times
/// `dt`, so 60 Hz and 120 Hz apply the same zoom in the same wall-clock time.
///
/// The rate also decides whether a *fast* sweep reaches the camera's own end: the share has
/// to be at least what one event of that sweep asks for, or the queue grows and the plate
/// lags the fingers. At 60 Hz the share is 13/60 = 0.217 log (1.24x), and a step of the
/// 40-step sweep in `tests/frame_cost.rs` asks for 0.21 at the accumulator's own gain — so
/// the two are within a whisker, and that sweep is where the rate is measured: at 13/s it
/// topped out at x89.3 of the x90 it needs (with the gentler of the two world gains), while
/// 20/s reached the ceiling in 14 of its 20 steps.
///
/// One frame before this rate existed, `views::map`'s own `ZOOM_STEP_MAX` allowed 0.35 log
/// (1.4x) — a third faster than 13/s at 60 Hz — so the transit here is gentler than the
/// window had and spreads a fast pinch over more frames.
pub const ZOOM_RATE: f32 = 13.0;

/// The longest frame a step will claim time for, in seconds (20 fps). A stalled frame
/// must not hand the camera five seconds of momentum in one go.
pub const DT_MAX: f32 = 0.05;

/// The shortest frame the *zoom* rate is computed from, in seconds (240 fps).
///
/// The share is `ZOOM_RATE * dt`, which is what makes 30, 60 and 120 Hz land on the same
/// zoom in the same wall-clock time. This is the floor under it: a frame that claims no
/// time at all — a synthetic stream, a window that reports `dt = 0` — still applies one
/// 240th of a second's worth, so a burst cannot be parked for ever on a display that
/// draws faster than the rate is written for.
pub const ZOOM_FLOOR_DT: f32 = 1.0 / 240.0;

/// How far the pointer must travel before a press is a pan, in points. Five is the
/// platform's own click tolerance; under it a press is a click that wobbled.
pub const SLOP: f32 = 5.0;

/// The most plate one pointer sample may move, in points.
///
/// A hand at the fastest flick this module allows (`FLICK_MAX`) covers 133 points in a
/// 30 Hz sample, so 200 is above anything a hand does and below a warp — a lost
/// pointer, a synthetic event, a window resized under the press. Without it one bad
/// sample teleports the plate a thousand points in a frame and then flings it onward
/// from the teleport's own speed.
pub const PAN_EVENT_MAX: f32 = 200.0;

/// How fast the flick estimate follows the pointer, per second: a 40 ms time constant,
/// so the speed that leaves a release is the speed of the last ~100 ms of motion.
pub const FLICK_RATE: f32 = 25.0;

/// The fastest flick a release may leave with, in points per second: the width of a
/// 1 000-point plate in a quarter of a second.
pub const FLICK_MAX: f32 = 4_000.0;

/// Momentum decay, per second: a 182 ms time constant, so a flick is over in about a
/// second instead of drifting, and it coasts at most `FLICK_MAX / MOMENTUM_RATE` = 727
/// points — long enough to feel released, short enough to stay on the plate.
pub const MOMENTUM_RATE: f32 = 5.5;

/// Under this speed (points per second) a coast is over rather than a crawl: the last
/// step is at most `MOMENTUM_MIN / MOMENTUM_RATE` = 2.2 points.
pub const MOMENTUM_MIN: f32 = 12.0;

/// The silence, in seconds, after which a zoom event starts a new gesture.
///
/// macOS delivers a pinch as bursts of ctrl-scroll events with gaps up to about 100 ms,
/// so a gesture has to survive a gap; a reader who stops pinching and starts again a
/// fifth of a second later means a new gesture, and half a second is where that is
/// certain. This is the one thing the wall clock is read for: a reactive window runs no
/// frame while the fingers are still, so a frame clock would never see the pause.
const GESTURE_GAP: f32 = 0.5;

/// Under this much pending zoom (log units) there is nothing left to apply. 1e-7 is a
/// hundredth of a pixel on a 1 000-point plate. It was 1e-4 — a tenth of a pixel, a
/// thousand times this — and a pending under it was neither spent nor cleared, so
/// `running` stayed true for ever and the window repainted for ever.
const ZOOM_EPS: f32 = 1e-7;

/// A pinch factor closer to 1.0 than this is no pinch at all.
///
/// It has to be a float noise floor, not a feel threshold. egui turns one point of
/// trackpad ctrl-scroll into `e^(1/200)` = 1.005 (`input_state/mod.rs:462-466`, with
/// `scroll_zoom_speed = 1/200` at `input_state/mod.rs:113`), so yesterday's 1e-3 dropped
/// every frame carrying less than a fifth of a point: a slow pinch lost 0.06 log — a
/// 6% zoom — over two seconds with nothing on screen to show for it. 1e-6 is a
/// four-hundredth of a point, below the noise of a product of two `f32`s.
const PINCH_EPS: f32 = 1e-6;

/// What one frame applies, in the view's own points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// Pan for this frame: a drag's movement, a coast's distance, or both.
    pub pan: Vec2,
    /// Zoom for this frame as a multiplier about [`Step::anchor`]; `1.0` is nothing.
    pub zoom: f32,
    /// Where the zoom is anchored — the pointer, or whatever the caller passed.
    pub anchor: Pos2,
    /// Something is still in flight, so the view must ask for another frame.
    pub running: bool,
}

/// The live press: where it started, where it is, and whether it has become a pan.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Press {
    origin: Pos2,
    at: Pos2,
    panning: bool,
}

/// Wheel, pinch, drag and flick, accumulated between frames and spent by them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Gesture {
    /// Zoom not yet applied, in log space, positive zooming in.
    pending: f32,
    /// Where the pending zoom is anchored: the pointer where its gesture *started*.
    anchor: Pos2,
    /// When the last zoom event arrived, for the gesture boundary.
    last: Option<Instant>,
    /// The press holding the canvas, if any.
    press: Option<Press>,
    /// Pan not yet spent by a step, in points.
    carry: Vec2,
    /// The pointer's speed while it is down, in points per second.
    flick: Vec2,
    /// The coast after the pointer let go, in points per second.
    vel: Vec2,
}

impl Gesture {
    /// One frame of zoom input. `scroll` is `smooth_scroll_delta.y` in points and `pinch`
    /// is `zoom_delta()`, which is `1.0` when there was no pinch.
    ///
    /// The pinch reading wins, and the scroll is the gesture only when there is no pinch. On
    /// egui 0.37 the two readings are already exclusive — a ctrl-scroll goes into
    /// `zoom_factor_delta` and leaves `smooth_scroll_delta` at zero
    /// (`input_state/mod.rs:462-472`) — but one frame can carry both a wheel event and an
    /// `Event::Zoom` (what egui-winit makes of a Mac magnify gesture), and a view that took
    /// both would apply that pinch twice.
    ///
    /// A whole gesture is spent around where it *started*: ten events of a burst are one
    /// intention, and pivoting the later half around a pointer that has since drifted
    /// would slide the plate under the fingers. After [`GESTURE_GAP`] of silence the
    /// next event is a new gesture and brings its own anchor.
    pub fn zoom(&mut self, scroll: f32, pinch: f32, at: Pos2) {
        let log = if let Some(pinch) = Gesture::pinch_reading(pinch) {
            pinch.ln().clamp(-PINCH_EVENT_MAX, PINCH_EVENT_MAX)
        } else if scroll.is_finite() && scroll != 0.0 {
            (scroll * WHEEL_GAIN).clamp(-WHEEL_EVENT_MAX, WHEEL_EVENT_MAX)
        } else {
            return;
        };
        self.zoom_log(log, at);
    }

    /// Is this frame's pinch reading a real pinch? `None` when it is the `1.0` egui
    /// reports for a frame that had none.
    ///
    /// A view asks before it treats the wheel as its own: a ctrl-scroll arrives in *both*
    /// readings, a page cannot be pinched, and a view that hands the scroll back to the
    /// page while a pinch is in flight would scroll the page under the fingers.
    pub fn pinch_reading(pinch: f32) -> Option<f32> {
        if pinch.is_finite() && (pinch - 1.0).abs() > PINCH_EPS {
            Some(pinch)
        } else {
            None
        }
    }

    /// Add a zoom the view has already measured, in log units — a keyboard's `+`, or a
    /// wheel the view first checked against its own clamp. Bounded per event like every
    /// other reading, so one impossible number cannot cross the plate.
    pub fn zoom_log(&mut self, log: f32, at: Pos2) {
        if !log.is_finite() || log == 0.0 {
            return;
        }
        let log = log.clamp(-PINCH_EVENT_MAX, PINCH_EVENT_MAX);
        let now = Instant::now();
        let fresh = self
            .last
            .map_or(true, |last| now.duration_since(last).as_secs_f32() >= GESTURE_GAP);
        if fresh {
            self.anchor = at;
        }
        self.last = Some(now);
        self.pending = (self.pending + log).clamp(-ZOOM_PENDING_MAX, ZOOM_PENDING_MAX);
        // a zoom is the reader saying "not that far": a coast stops under it
        self.vel = Vec2::ZERO;
    }

    /// One frame of pointer input: `down` while a button is held on the view, `at`
    /// wherever the pointer is (`pointer.latest_pos()`, not `hover_pos()`, so a drag
    /// that leaves the widget keeps its position).
    ///
    /// Called every frame, so a release cannot be missed: a view that forgot it would
    /// coast forever.
    pub fn pointer(&mut self, down: bool, at: Pos2) {
        match (down, self.press.is_some()) {
            (true, false) => {
                // catching the canvas stops a coast: a reader who grabs a moving map
                // means it, and a click must never inherit a fling
                self.press = Some(Press { origin: at, at, panning: false });
                self.flick = Vec2::ZERO;
                self.vel = Vec2::ZERO;
            }
            (true, true) => self.move_to(at),
            (false, true) => {
                // the movement of the last frame before the release is still a pan
                self.move_to(at);
                self.release();
            }
            (false, false) => {}
        }
    }

    /// The pointer let go (or a view-measured pan ended): what was moving becomes a
    /// coast, decaying by time. A press that never crossed [`SLOP`] has no flick of its
    /// own, so a click that wobbled leaves the camera exactly where it was.
    ///
    /// Public because a view replaying a pan of its own ([`Gesture::panned`]) has no
    /// press for [`Gesture::pointer`] to release.
    pub fn release(&mut self) {
        self.press = None;
        self.vel = clamp_speed(self.flick);
        self.flick = Vec2::ZERO;
    }

    /// A pan the view measured itself, in points: egui's `response.drag_delta()` on a
    /// canvas the view owns, a keyboard nudge, a drag a test replays.
    ///
    /// The view has already moved its own camera by `by` — this does not pan, it
    /// *records*: the one thing the accumulator needs from a pan is the speed a
    /// release coasts with, and the one thing a pan means is "not that zoom". `dt` is
    /// the frame the movement happened in; zero for a discrete step (a key press, a
    /// test replaying a drag with no clock), which is a step and not a throw.
    pub fn panned(&mut self, by: Vec2, dt: f32) {
        // a drag is the reader saying "not that zoom", like a wheel is
        self.pending = 0.0;
        if dt > 0.0 && by.is_finite() {
            // The sample *is* this frame's speed: one `drag_delta` is already the
            // whole movement since the last frame, so smoothing it against the
            // estimate would only make a release coast slower than the hand moved.
            self.flick = bounded(by / dt, FLICK_MAX);
        }
    }

    /// The coast the accumulator is running, in points per second. Read by the map's
    /// momentum test, which is about distances and speeds rather than about pixels.
    pub fn coast(&self) -> Vec2 {
        self.vel
    }

    /// Is the press a pan yet? A view asks before it treats the frame as a click.
    pub fn is_panning(&self) -> bool {
        self.press.map(|press| press.panning).unwrap_or(false)
    }

    /// The zoom this gesture still owes the camera, in log units.
    ///
    /// A view reads it to decide whether its own clamp has room for one more event: a
    /// plate already at its floor has nothing to spend, and the wheel is the page's.
    pub fn pending_zoom(&self) -> f32 {
        self.pending
    }

    /// Drop everything in flight: the zoom that has not been spent and the coast that has
    /// not been run out. The press is left alone — a reader holding the plate is still
    /// holding it.
    pub fn forget(&mut self) {
        self.pending = 0.0;
        self.vel = Vec2::ZERO;
    }

    /// Is anything still in flight — a burst to spend, a coast to run out, a press that is
    /// a pan? A reactive window asks this to know whether it must draw another frame.
    pub fn is_moving(&self) -> bool {
        self.pending != 0.0 || self.vel != Vec2::ZERO || self.is_panning()
    }

    /// Fold one pointer sample into the press. The press becomes a pan when it has
    /// travelled [`SLOP`] points away from where it landed, and from then on every
    /// point counts: the pan starts at the press rather than at the crossing, so the
    /// picture never trails the finger by the slop distance.
    ///
    /// One sample moves the plate at most [`PAN_EVENT_MAX`] points, so the clamp is on
    /// the event and not on the frame: a coalesced burst of honest samples adds up to
    /// the honest total, while a single impossible one cannot cross the plate.
    fn move_to(&mut self, at: Pos2) {
        let Some(mut press) = self.press else { return };
        if !press.panning {
            if (at - press.origin).length() <= SLOP {
                press.at = at;
                self.press = Some(press);
                return;
            }
            press.panning = true;
            self.carry += bounded(at - press.origin, PAN_EVENT_MAX);
        } else {
            self.carry += bounded(at - press.at, PAN_EVENT_MAX);
        }
        press.at = at;
        self.press = Some(press);
    }

    /// Spend one frame. `dt` is the frame's own time in seconds (`stable_dt`).
    pub fn step(&mut self, dt: f32) -> Step {
        let dt = if dt.is_finite() { dt.clamp(0.0, DT_MAX) } else { 0.0 };

        // Everything the pointer did since the last frame is a pan, and the same
        // distance per second is what a flick would carry.
        let mut pan = std::mem::take(&mut self.carry);
        let target = match self.press {
            Some(press) if press.panning && dt > 0.0 => pan / dt,
            _ => Vec2::ZERO,
        };
        // Weighted by dt, so 60 Hz and 120 Hz estimate the same speed from the same
        // hand movement.
        self.flick += (target - self.flick) * (1.0 - (-FLICK_RATE * dt).exp());

        // The coast. `(1 - e^(-r*dt)) / r` is the exact integral of the decaying
        // velocity over the frame, not a rectangle: the sums telescope, so the flick
        // travels the same distance in the same second at any frame rate.
        let mut running = false;
        if self.press.is_none() && self.vel != Vec2::ZERO {
            let decay = (-MOMENTUM_RATE * dt).exp();
            pan += self.vel * ((1.0 - decay) / MOMENTUM_RATE);
            self.vel *= decay;
            if self.vel.length() <= MOMENTUM_MIN {
                self.vel = Vec2::ZERO;
            }
            running = true;
        }

        // The zoom: one frame gets `ZOOM_RATE * dt` of the burst and no more, so a
        // burst of ten events takes ten frames at a rate the eye follows.
        let mut zoom = 1.0;
        // A rate per second, with the floor under `dt` that keeps a fast display from
        // parking a burst: see `ZOOM_FLOOR_DT`. A frame that claims no time at all is no
        // frame, and spends nothing.
        let share = if dt > 0.0 { ZOOM_RATE * dt.max(ZOOM_FLOOR_DT) } else { 0.0 };
        if self.pending.abs() > ZOOM_EPS && share > 0.0 {
            let spent = self.pending.clamp(-share, share);
            self.pending -= spent;
            if self.pending.abs() <= ZOOM_EPS {
                self.pending = 0.0;
            }
            zoom = spent.exp();
            running = true;
        }

        Step {
            pan,
            zoom,
            anchor: self.anchor,
            running: running || self.pending != 0.0 || self.is_panning(),
        }
    }
}

/// Clamp a velocity to what a flick may leave with, and forget a crawl.
fn clamp_speed(v: Vec2) -> Vec2 {
    let speed = v.length();
    if !speed.is_finite() || speed <= MOMENTUM_MIN {
        return Vec2::ZERO;
    }
    bounded(v, FLICK_MAX)
}

/// Shorten `v` to `max`, keeping its direction. A vector that is not a number is no
/// movement at all.
fn bounded(v: Vec2, max: f32) -> Vec2 {
    let length = v.length();
    if !length.is_finite() {
        return Vec2::ZERO;
    }
    if length > max {
        v * (max / length)
    } else {
        v
    }
}

/// Apply one frame's zoom to a camera of the shape `screen = centre + world * k + pan`.
///
/// Both views already keep their camera that way — the map flips the world's latitude
/// axis before it reaches the screen, and this does not care, because the axes are
/// independent. Scaling `k` by `factor` pushes every point away from `centre`, so the
/// translation is re-derived to put the world point under `anchor` back under it; a
/// pinch that slid the picture would be worse than one that did nothing. `bounds` is
/// the zoom clamp as `(min, max)`; the pan clamp stays with the view, because only the
/// view knows how far its world may be dragged.
pub fn zoom_about(
    pan: Vec2,
    k: f32,
    factor: f32,
    anchor: Pos2,
    centre: Pos2,
    bounds: (f32, f32),
) -> (Vec2, f32) {
    if !(k > 0.0) || !k.is_finite() || !(factor > 0.0) || !factor.is_finite() {
        return (pan, k);
    }
    let (low, high) = (bounds.0.min(bounds.1), bounds.0.max(bounds.1));
    let scaled = (k * factor).clamp(low, high);
    if (scaled - k).abs() <= f32::EPSILON * k {
        return (pan, scaled);
    }
    let offset = anchor - centre;
    let ratio = scaled / k;
    (offset - (offset - pan) * ratio, scaled)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Pos2 = Pos2::new(300.0, 200.0);

    /// Step until nothing is in flight, and answer how much zoom was spent (log) and
    /// how many frames it took.
    fn drain(gesture: &mut Gesture, dt: f32) -> (f32, usize) {
        let mut log = 0.0;
        let mut frames = 0;
        loop {
            let step = gesture.step(dt);
            log += step.zoom.ln();
            frames += 1;
            if !step.running {
                return (log, frames);
            }
            assert!(frames < 500, "a gesture must be spent, not kept");
        }
    }

    #[test]
    fn a_slow_pinch_is_not_dropped_for_being_small() {
        // egui turns one point of trackpad ctrl-scroll into `e^(1/200)`: a gentle pinch
        // frame carries a tenth of a point, a factor of 1.0005. A feel threshold of a
        // tenth of a percent (the old PINCH_EPS) dropped every one of those, and 120 of
        // them — two seconds of a slow pinch — carry 0.06 log, a 6% zoom, invisibly.
        let gentle = (0.1_f32 / 200.0).exp();
        let mut gesture = Gesture::default();
        for _ in 0..120 {
            gesture.zoom(0.0, gentle, AT);
        }
        let (log, _) = drain(&mut gesture, 1.0 / 60.0);
        let wanted = 120.0 * gentle.ln();
        assert!(wanted > 0.05, "the stream carries {wanted} log");
        assert!((log - wanted).abs() < 1e-4, "the slow pinch spent {log} of {wanted}");
    }

    #[test]
    fn a_sub_pixel_pending_is_spent_and_never_parked() {
        // A twentieth of a point is 7.5e-5 log — under a tenth of a pixel on a 1 000
        // point plate. It used to be under ZOOM_EPS (1e-4), where it was neither spent
        // nor cleared: the gesture reported `running` for ever and the window repainted
        // for ever. It has to be applied, and the gesture has to end.
        let mut gesture = Gesture::default();
        gesture.zoom(0.05, 1.0, AT);
        let (log, frames) = drain(&mut gesture, 1.0 / 60.0);
        // 1e-7 of a log unit is a hundredth of a pixel: the tolerance is the `exp`/`ln`
        // round trip, not the accumulator's own arithmetic
        assert!((log - 0.05 * WHEEL_GAIN).abs() < 1e-7, "the twentieth of a point spent {log}");
        assert!(log > 0.0, "a sub-pixel event was dropped: {log}");
        assert!(frames <= 3, "a sub-pixel event took {frames} frames");
    }

    #[test]
    fn a_warp_of_the_pointer_moves_the_plate_one_event_s_worth() {
        // A lost pointer, a synthetic event or a window resized under the press reports
        // a sample thousands of points away. A hand at FLICK_MAX covers 133 points in a
        // 30 Hz sample, so 200 is the most one sample may move the plate — and the
        // coast must not inherit the warp's speed either.
        let mut gesture = Gesture::default();
        gesture.pointer(true, Pos2::new(100.0, 100.0));
        gesture.pointer(true, Pos2::new(5_100.0, 100.0));
        let step = gesture.step(1.0 / 30.0);
        assert_eq!(step.pan.x, PAN_EVENT_MAX, "a warp moved the plate {}", step.pan.x);
        gesture.pointer(false, Pos2::new(5_100.0, 100.0));
        let mut coast = 0.0;
        for _ in 0..600 {
            let step = gesture.step(1.0 / 30.0);
            coast += step.pan.x;
            if !step.running {
                break;
            }
        }
        assert!(coast > 0.0 && coast <= FLICK_MAX / MOMENTUM_RATE + 1e-3, "the coast was {coast}");
    }

    #[test]
    fn a_burst_is_anchored_where_it_started() {
        // Ten events between two frames are one intention. Anchoring each around its own
        // position would pivot the later half of the burst around a pointer that has
        // moved, and the plate would slide under the fingers.
        let first = Pos2::new(220.0, 140.0);
        let mut gesture = Gesture::default();
        gesture.zoom(6.0, 1.0, first);
        gesture.zoom(6.0, 1.0, Pos2::new(260.0, 140.0));
        gesture.zoom(6.0, 1.0, Pos2::new(300.0, 140.0));
        assert_eq!(gesture.step(1.0 / 60.0).anchor, first);
        // and a frame later, the same gesture keeps it
        gesture.zoom(6.0, 1.0, Pos2::new(340.0, 140.0));
        assert_eq!(gesture.step(1.0 / 60.0).anchor, first);
    }

    #[test]
    fn a_burst_of_events_is_one_gesture_of_the_same_total() {
        // Twenty trackpad events of 6 points, or one event of 120: the same fingers,
        // the same distance, and the same zoom once the accumulator has spent it.
        let mut burst = Gesture::default();
        for _ in 0..20 {
            burst.zoom(6.0, 1.0, AT);
        }
        let mut single = Gesture::default();
        single.zoom(120.0, 1.0, AT);
        assert!((burst.pending - single.pending).abs() < 1e-6, "{burst:?} {single:?}");

        let (burst_log, burst_frames) = drain(&mut burst, 1.0 / 120.0);
        let (single_log, single_frames) = drain(&mut single, 1.0 / 120.0);
        assert!((burst_log - single_log).abs() < 1e-6, "{burst_log} vs {single_log}");
        assert!((burst_log - 120.0 * WHEEL_GAIN).abs() < 1e-6);
        assert_eq!(burst_frames, single_frames, "the same total takes the same frames");
        assert!(burst_frames > 1, "a burst is spread over frames, not applied at once");

        // A pinch is additive for the same reason: five factors of 1.05 are one of
        // 1.05^5, and they are spent identically.
        let mut pinched = Gesture::default();
        for _ in 0..5 {
            pinched.zoom(0.0, 1.05, AT);
        }
        let mut one_pinch = Gesture::default();
        one_pinch.zoom(0.0, 1.05_f32.powi(5), AT);
        let (pinched_log, _) = drain(&mut pinched, 1.0 / 60.0);
        let (one_log, _) = drain(&mut one_pinch, 1.0 / 60.0);
        assert!((pinched_log - one_log).abs() < 1e-6, "{pinched_log} vs {one_log}");
        assert!(pinched_log > 0.2, "spreading five times is a visible zoom");
    }

    #[test]
    fn the_ease_is_the_same_at_sixty_and_at_a_hundred_and_twenty_hertz() {
        // The same burst, then a tenth of a second of frames each way: the rate can
        // spend 1.3 of the 2.4 that is pending, so this is the rate binding and not
        // the burst running out.
        let burst = |gesture: &mut Gesture| {
            for _ in 0..20 {
                gesture.zoom(100_000.0, 1.0, AT);
            }
        };
        let mut fast = Gesture::default();
        burst(&mut fast);
        let mut slow = Gesture::default();
        burst(&mut slow);
        assert_eq!(fast.pending, ZOOM_PENDING_MAX, "the pending total is clamped");

        let mut spent_fast = 0.0;
        for _ in 0..12 {
            spent_fast += fast.step(1.0 / 120.0).zoom.ln();
        }
        let mut spent_slow = 0.0;
        for _ in 0..6 {
            spent_slow += slow.step(1.0 / 60.0).zoom.ln();
        }
        let wanted = ZOOM_RATE * 0.1;
        assert!(
            (spent_fast - wanted).abs() < 1e-3,
            "120 Hz spent {spent_fast} of {wanted} in a tenth of a second"
        );
        assert!(
            (spent_slow - wanted).abs() < 1e-3,
            "60 Hz spent {spent_slow} of {wanted} in a tenth of a second"
        );

        // 30 Hz is the third rate a window can run at, and the same tenth of a second has to
        // be the same zoom there too: `ZOOM_RATE * dt` is 0.433, so one frame spends more than
        // the 0.4 a gesture can queue in one event and the 30 Hz path is bounded by the burst.
        let mut thirty = Gesture::default();
        for _ in 0..20 {
            thirty.zoom(100_000.0, 1.0, AT);
        }
        let mut spent_thirty = 0.0;
        for _ in 0..3 {
            spent_thirty += thirty.step(1.0 / 30.0).zoom.ln();
        }
        let wanted_thirty = ZOOM_RATE * 0.1;
        assert!(
            (spent_thirty - wanted_thirty).abs() < 1e-3,
            "30 Hz spent {spent_thirty} of {wanted_thirty} in a tenth of a second"
        );

        // And one frame applies its own share and no more: half the time, half the zoom.
        let mut one = Gesture::default();
        one.zoom(100_000.0, 1.0, AT);
        let at_60 = one.step(1.0 / 60.0).zoom.ln();
        let mut one = Gesture::default();
        one.zoom(100_000.0, 1.0, AT);
        let at_120 = one.step(1.0 / 120.0).zoom.ln();
        assert!((at_60 - 2.0 * at_120).abs() < 1e-6, "{at_60} vs {at_120}");

        // A stalled frame is clamped to DT_MAX, not to the whole stall.
        let mut stalled = Gesture::default();
        stalled.zoom(100_000.0, 1.0, AT);
        let step = stalled.step(5.0);
        assert!(
            step.zoom.ln() <= ZOOM_RATE * DT_MAX + 1e-6,
            "a five-second frame applied {}",
            step.zoom.ln()
        );
    }

    #[test]
    fn a_flick_carries_on_and_then_stops() {
        let mut gesture = Gesture::default();
        let mut at = Pos2::new(100.0, 100.0);
        gesture.pointer(true, at);
        // one second of a fast drag: 40 points per 60 Hz frame, 2400 points a second
        let mut dragged = Vec2::ZERO;
        for _ in 0..60 {
            at.x += 40.0;
            gesture.pointer(true, at);
            dragged += gesture.step(1.0 / 60.0).pan;
        }
        assert_eq!(dragged.x, 2400.0, "a drag is 1:1 with the pointer");
        assert!(gesture.is_panning());

        gesture.pointer(false, at);
        let mut coast = Vec2::ZERO;
        let mut frames = 0;
        loop {
            let step = gesture.step(1.0 / 60.0);
            coast += step.pan;
            frames += 1;
            if !step.running {
                assert_eq!(step.pan, Vec2::ZERO, "the coast ends exactly, not as a crawl");
                break;
            }
            assert!(frames < 300, "a flick must stop: {coast:?}");
        }
        assert!(coast.x > 300.0, "the flick carried on: {coast:?}");
        assert!(coast.x <= FLICK_MAX / MOMENTUM_RATE, "the coast is bounded: {coast:?}");
        assert!(frames < 120, "it stopped in {frames} frames");
    }

    #[test]
    fn a_flick_travels_the_same_distance_at_sixty_and_at_a_hundred_and_twenty_hertz() {
        let coast = |dt: f32| {
            let mut gesture = Gesture::default();
            let mut at = Pos2::new(100.0, 100.0);
            gesture.pointer(true, at);
            // one second of identical hand movement, sampled at this frame rate
            let frames = (1.0 / dt).round() as usize;
            let per_frame = 2400.0 * dt;
            for _ in 0..frames {
                at.x += per_frame;
                gesture.pointer(true, at);
                gesture.step(dt);
            }
            gesture.pointer(false, at);
            let mut moved = Vec2::ZERO;
            for _ in 0..600 {
                let step = gesture.step(dt);
                moved += step.pan;
                if !step.running {
                    break;
                }
            }
            moved.x
        };
        let slow = coast(1.0 / 60.0);
        let fast = coast(1.0 / 120.0);
        assert!(slow > 200.0, "the coast is real: {slow}");
        // The decay is integrated exactly, so the two differ only by the last partial
        // step before the floor: at most MOMENTUM_MIN / MOMENTUM_RATE ≈ 2 points.
        assert!(
            (slow - fast).abs() < slow * 1e-2,
            "60 Hz coasted {slow}, 120 Hz coasted {fast}"
        );
    }

    #[test]
    fn a_drag_under_the_slop_is_not_a_pan() {
        let mut gesture = Gesture::default();
        gesture.pointer(true, Pos2::new(200.0, 150.0));
        gesture.pointer(true, Pos2::new(202.0, 152.0)); // 2.8 points: a click
        let step = gesture.step(1.0 / 60.0);
        assert_eq!(step.pan, Vec2::ZERO, "a wobbling click must not move the camera");
        assert!(!gesture.is_panning());

        // crossing it pans by the whole movement since the press, so the map never
        // trails the finger by the slop
        gesture.pointer(true, Pos2::new(210.0, 150.0)); // 10 points from the press
        assert!(gesture.is_panning());
        assert_eq!(gesture.step(1.0 / 60.0).pan, Vec2::new(10.0, 0.0));

        // and a press that never crossed it leaves no flick behind
        let mut click = Gesture::default();
        click.pointer(true, Pos2::new(0.0, 0.0));
        click.pointer(true, Pos2::new(3.0, 3.0));
        click.pointer(false, Pos2::new(3.0, 3.0));
        let step = click.step(1.0 / 60.0);
        assert_eq!(step.pan, Vec2::ZERO);
        assert!(!step.running, "a click leaves nothing in flight");
    }

    #[test]
    fn one_ctrl_scroll_is_one_zoom_not_two() {
        // egui reports a ctrl-scroll twice: 120 points of scroll and the same event as
        // a factor of e^(120 * 0.0015). Taking both would double the pinch.
        let mut gesture = Gesture::default();
        gesture.zoom(120.0, (120.0 * WHEEL_GAIN).exp(), AT);
        assert!(
            (gesture.pending - 120.0 * WHEEL_GAIN).abs() < 1e-4,
            "the pinch was counted twice: {}",
            gesture.pending
        );
        // the plain scroll has no pinch reading, so it is the gesture
        let mut plain = Gesture::default();
        plain.zoom(-120.0, 1.0, AT);
        assert!((plain.pending + 120.0 * WHEEL_GAIN).abs() < 1e-4);
        assert_eq!(plain.anchor, AT, "the zoom is anchored where the pointer is");
    }

    #[test]
    fn every_request_is_clamped() {
        // one event
        let mut gesture = Gesture::default();
        gesture.zoom(100_000.0, 1.0, AT);
        assert_eq!(gesture.pending, WHEEL_EVENT_MAX);
        let mut gesture = Gesture::default();
        gesture.zoom(0.0, 1e6, AT);
        assert_eq!(gesture.pending, PINCH_EVENT_MAX);
        // the pending total
        let mut gesture = Gesture::default();
        for _ in 0..50 {
            gesture.zoom(100_000.0, 1.0, AT);
        }
        assert_eq!(gesture.pending, ZOOM_PENDING_MAX);
        // a nonsense frame
        let mut gesture = Gesture::default();
        gesture.zoom(100.0, 1.0, AT);
        assert_eq!(gesture.step(f32::NAN).zoom, 1.0, "no time, no motion");
        assert_eq!(gesture.step(-1.0).zoom, 1.0, "and a backwards clock is no time");
    }

    #[test]
    fn a_zoom_keeps_the_point_under_the_pointer_under_it() {
        let centre = Pos2::new(500.0, 260.0);
        let (pan, k) = (Vec2::new(0.0, 0.0), 3.0_f32);
        let anchor = Pos2::new(520.0, 300.0);
        let world = |pan: Vec2, k: f32| {
            let offset = anchor - centre;
            ((offset.x - pan.x) / k, (offset.y - pan.y) / k)
        };
        let before = world(pan, k);
        let (pan2, k2) = zoom_about(pan, k, 1.5, anchor, centre, (0.2, 240.0));
        let after = world(pan2, k2);
        assert!((k2 - 4.5).abs() < 1e-6);
        assert!((before.0 - after.0).abs() < 1e-5 && (before.1 - after.1).abs() < 1e-5);

        // the scale is clamped, and a clamp is not a pan: the anchor still holds
        let (_, deep) = zoom_about(pan, 200.0, 10.0, anchor, centre, (0.2, 240.0));
        assert_eq!(deep, 240.0);
        let (_, close) = zoom_about(pan, 1.0, 0.001, anchor, centre, (0.2, 240.0));
        assert_eq!(close, 0.2);
        // nothing to do is nothing done
        assert_eq!(zoom_about(pan, k, 1.0, anchor, centre, (0.2, 240.0)), (pan, k));
    }

    /// A camera driven the way a view drives it: one frame, one step, one clamp — the
    /// order `views/map.rs` and `views/graph.rs` spend a gesture in.
    struct Plate {
        pan: Vec2,
        k: f32,
        centre: Pos2,
        bounds: (f32, f32),
    }

    impl Plate {
        /// The Overview's map plate: 1 000 x 470 points, whose fill is
        /// `(1000/360).min(470/180) * 0.94` = 2.455 points per degree.
        const FILL: f32 = 2.455;

        fn new(k: f32, bounds: (f32, f32)) -> Plate {
            Plate { pan: Vec2::ZERO, k, centre: Pos2::new(500.0, 235.0), bounds }
        }

        /// One frame: exactly what a view does with a [`Step`].
        fn frame(&mut self, gesture: &mut Gesture, dt: f32) -> Step {
            let step = gesture.step(dt);
            let (pan, k) =
                zoom_about(self.pan, self.k, step.zoom, step.anchor, self.centre, self.bounds);
            self.pan = pan + step.pan;
            self.k = k;
            step
        }

        /// Spend the gesture to rest, and answer how many frames it took.
        fn settle(&mut self, gesture: &mut Gesture, dt: f32) -> usize {
            let mut frames = 0;
            loop {
                let step = self.frame(gesture, dt);
                frames += 1;
                if !step.running {
                    return frames;
                }
                assert!(frames < 5_000, "a gesture must settle, not creep");
            }
        }
    }

    #[test]
    fn a_gesture_settles_inside_the_ease_and_then_leaves_the_scale_alone() {
        // The most a gesture may queue is ZOOM_PENDING_MAX = 2.4 log, and the most the
        // ease spends is ZOOM_RATE = 13 log/s, so a full burst is spent in
        // ceil(2.4 / (13/60)) = 12 frames at 60 Hz and ceil(2.4 / (13/120)) = 23 at
        // 120 Hz — plus the one frame that then finds nothing left. After that the
        // scale must not move again: a remainder, a latched velocity or a re-queued
        // event would show up as a plate still drifting a second later.
        let mut gesture = Gesture::default();
        for _ in 0..20 {
            gesture.zoom(100_000.0, 1.0, AT);
        }
        let mut plate = Plate::new(Plate::FILL, (0.4, 400.0));
        let frames = plate.settle(&mut gesture, 1.0 / 60.0);
        assert!(frames <= 13, "a full burst took {frames} frames at 60 Hz, wanted 13");

        let target = Plate::FILL * ZOOM_PENDING_MAX.exp();
        assert!(
            (plate.k - target).abs() <= target * 1e-4,
            "the plate settled at {} instead of {target}",
            plate.k
        );
        let settled = plate.k;
        for frame in 0..60 {
            let step = plate.frame(&mut gesture, 1.0 / 60.0);
            assert!(!step.running, "frame {frame} after the ease: still running");
            assert_eq!(step.zoom, 1.0, "frame {frame} after the ease still zoomed");
            assert_eq!(
                plate.k, settled,
                "frame {frame} after the ease: the scale crept to {}",
                plate.k
            );
        }

        // The same bound counts up with the frame rate, so the burst lasts the same
        // 200 ms on a 120 Hz panel as on a 60 Hz one.
        let mut gesture = Gesture::default();
        for _ in 0..20 {
            gesture.zoom(100_000.0, 1.0, AT);
        }
        let mut plate = Plate::new(Plate::FILL, (0.4, 400.0));
        let frames = plate.settle(&mut gesture, 1.0 / 120.0);
        assert!(frames <= 25, "a full burst took {frames} frames at 120 Hz, wanted 25");
    }

    #[test]
    fn a_zoom_in_and_the_same_zoom_out_land_on_the_same_scale() {
        // "Sometimes it does not scale back exactly": a pending remainder left behind,
        // or a burst clipped on the way in and not on the way out, leaves the plate a
        // fraction of a percent off — read as a world that never quite comes back to
        // the fit. Away from the clamp, two opposite bursts of the same size must
        // cancel to within a thousandth at every scale the map is used at.
        let bounds = (0.4, 400.0);
        for start in [Plate::FILL, 10.0, 90.0] {
            let mut gesture = Gesture::default();
            let mut plate = Plate::new(start, bounds);
            for _ in 0..8 {
                gesture.zoom(40.0, 1.0, AT);
            }
            plate.settle(&mut gesture, 1.0 / 60.0);
            let peak = plate.k;
            assert!(peak > start * 1.2, "the burst zoomed in at {start}: {peak}");
            for _ in 0..8 {
                gesture.zoom(-40.0, 1.0, AT);
            }
            plate.settle(&mut gesture, 1.0 / 60.0);
            assert!(
                (plate.k - start).abs() <= start * 1e-3,
                "from {start} the plate came back to {}, {:.3}% off",
                plate.k,
                (plate.k / start - 1.0) * 100.0
            );
        }

        // And a pinch that goes straight back before the ease has finished cancels in
        // the queue, not on the screen: the plate ends where it started.
        let mut gesture = Gesture::default();
        let mut plate = Plate::new(10.0, bounds);
        for _ in 0..8 {
            gesture.zoom(40.0, 1.0, AT);
        }
        plate.frame(&mut gesture, 1.0 / 60.0);
        let on_its_way = plate.k;
        for _ in 0..8 {
            gesture.zoom(-40.0, 1.0, AT);
        }
        plate.settle(&mut gesture, 1.0 / 60.0);
        assert!(on_its_way > 10.0, "the way in never started: {on_its_way}");
        assert!(
            (plate.k - 10.0).abs() <= 10.0 * 1e-3,
            "the way back left the plate at {}",
            plate.k
        );
    }

    #[test]
    fn a_burst_the_clamp_refused_is_spent_and_never_banked() {
        // At the ceiling the camera cannot move, but the burst is still spent a share at
        // a time. If it were kept, the next pinch *out* would first have to unwind 2.4
        // log of banked zoom: the plate would sit still under the fingers and then jump,
        // which is the other half of "zooming feels wrong".
        let bounds = (0.4, 400.0);
        let mut gesture = Gesture::default();
        let mut plate = Plate::new(400.0, bounds);
        for _ in 0..20 {
            gesture.zoom(100_000.0, 1.0, AT);
        }
        plate.settle(&mut gesture, 1.0 / 60.0);
        assert_eq!(plate.k, 400.0, "the ceiling is not a spring");
        gesture.zoom(-40.0, 1.0, AT);
        plate.settle(&mut gesture, 1.0 / 60.0);
        let wanted = 400.0 * (-40.0 * WHEEL_GAIN).exp();
        assert!(
            (plate.k - wanted).abs() <= wanted * 1e-3,
            "one notch out of the ceiling landed on {} instead of {wanted}",
            plate.k
        );

        // Mirrored at the floor: a burst out is lost there, and it is not replayed as a
        // debt either — the burst in that follows lands on its own full total.
        let mut gesture = Gesture::default();
        let mut plate = Plate::new(0.4, bounds);
        for _ in 0..20 {
            gesture.zoom(-100_000.0, 1.0, AT);
        }
        plate.settle(&mut gesture, 1.0 / 60.0);
        assert_eq!(plate.k, 0.4, "the floor holds");
        for _ in 0..8 {
            gesture.zoom(40.0, 1.0, AT);
        }
        plate.settle(&mut gesture, 1.0 / 60.0);
        let wanted = 0.4 * (8.0 * 40.0 * WHEEL_GAIN).exp();
        assert!(
            (plate.k - wanted).abs() <= wanted * 1e-3,
            "the floor banked a debt: {} instead of {wanted}",
            plate.k
        );
    }
}
