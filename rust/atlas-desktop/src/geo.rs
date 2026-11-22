//! The map's projection: degrees in, box pixels out, and the arithmetic a camera
//! needs to move without losing the place under the pointer.
//!
//! Equirectangular, not Mercator: the vendored outlines are longitude/latitude
//! already, so drawing one is a multiplication, the inverse is a division, and
//! nothing in this app measures distance on the map. The bill comes due at the top
//! of the plate, where Greenland is drawn as wide as Africa — the same bill the
//! raster pyramid paid, and the same shapes it drew.
//!
//! Three spaces, named once:
//!
//! * *degrees* — a [`Geo`], longitude east, latitude north, the plate being 360 by
//!   180 of them;
//! * *the box* — the rectangle the view allocates for the map, in pixels;
//! * *view pixels* — a [`Point`], measured from the box's centre, `y` down.
//!
//! Anything here is a value: no egui type, no painter, no clock, no state shared
//! with a view. A [`Camera`] can be copied, clamped and asserted on, which is why
//! the anchor invariant and the plate edge can be tests instead of experiments.

// ---------------------------------------------------------------- the plate

/// The plate, in degrees: the width of the world and the height of it.
const PLATE_W: f32 = 360.0;
const PLATE_H: f32 = 180.0;

/// The deepest zoom, as a multiple of the zoom that fills the box.
///
/// 96× puts 267 pixels on the degree of a 1 000 px card: one pixel is 420 m, a place
/// named to a kilometre still sits 2.4 px from its neighbour, and the 1:50m
/// geometry's median 0.09° segment is 25 px long. Past that, the only thing still
/// growing is the source's own faceting.
const MAX_ZOOM: f32 = 96.0;

/// The hairline a coastline is stroked with, in pixels. A ring thinner than the line
/// it would be drawn with is one step of grey, not a shape.
const HAIRLINE_PX: f32 = 1.0;

/// The largest distance, in pixels, that a dropped vertex may sit off the line
/// replacing it. Half the hairline: a coast thinned at this tolerance uses the same
/// pixels as the full ring, so the count can fall by an order of magnitude without
/// the outline changing.
const TOLERANCE_PX: f32 = 0.5;

/// A place in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub lon: f32,
    pub lat: f32,
}

impl Geo {
    pub const fn new(lon: f32, lat: f32) -> Self {
        Geo { lon, lat }
    }
}

/// A point in view pixels, measured from the box's centre: `x` right, `y` down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
}

// ------------------------------------------------------------------- camera

/// One side of the box, or 1 px when the number cannot describe a box.
///
/// `f32::max` already turns a NaN into the other operand, so zero, negative and NaN
/// sizes are one case; an *infinite* size is the one that survives `max(1.0)` and then
/// poisons every projection through `∞/∞`.
fn finite_or_one(side: f32) -> f32 {
    if side.is_finite() {
        side.max(1.0)
    } else {
        1.0
    }
}

/// Where the plate sits in the box, and how big a degree is.
///
/// Two properties hold after every method that moves it, and the tests below are
/// what "every" means:
///
/// * the scale stays between the zoom that fills the box and [`MAX_ZOOM`] times it
///   — the first so the plate is never letterboxed, the second because deeper shows
///   nothing new;
/// * the plate covers the box — each axis stopped with its edge on the box's edge,
///   so the world cannot be dragged off the card and no air appears beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// The box the plate is drawn into, in pixels.
    w: f32,
    h: f32,
    /// The plate's centre, in view pixels.
    x: f32,
    y: f32,
    /// Pixels per degree. Latitude uses the same scale, so the plate keeps its 2:1
    /// shape and a square degree is a square on screen.
    k: f32,
}

impl Camera {
    /// The whole plate, filling `size`. This is the zoom a map opens at: flush with
    /// the box on the tighter axis, because a margin around the world is air, and
    /// the card this is drawn in has one already.
    pub fn new(size: (f32, f32)) -> Self {
        let mut camera = Camera { w: 1.0, h: 1.0, x: 0.0, y: 0.0, k: 0.0 };
        camera.resize(size);
        camera
    }

    /// The box changed: keep the centre and the zoom, and seat the plate in the new
    /// box. A window that grows never opens a gap around the world, and a window
    /// that shrinks never keeps a scale the plate can no longer fill.
    pub fn resize(&mut self, size: (f32, f32)) {
        // `max(1.0)` alone keeps an infinity (`INFINITY.max(1.0) == INFINITY`), and an
        // infinite box makes the fill infinite, the scale infinite and `zoom()` an
        // `∞/∞` — that camera projects every place to NaN. A NaN box is already caught
        // by `f32::max` returning the finite operand; this is the one value that slips
        // through, so it is spelled out.
        self.w = finite_or_one(size.0);
        self.h = finite_or_one(size.1);
        if !self.k.is_finite() || self.k <= 0.0 {
            self.k = self.fill();
        }
        self.settle();
    }

    /// Drag the plate by a pixel delta.
    pub fn pan(&mut self, delta: (f32, f32)) {
        if !(delta.0.is_finite() && delta.1.is_finite()) {
            return;
        }
        self.x += delta.0;
        self.y += delta.1;
        self.settle();
    }

    /// Zoom by `factor` about `anchor`, so the place under the anchor stays under
    /// it — the pointer keeps what it was pointing at.
    ///
    /// The scale is clamped first and the centre is then derived from the anchor at
    /// that scale, so the anchor holds even when the gesture is at one end of the
    /// zoom. Only the plate's own edge can move it: a plate flush with the box has
    /// no slack, and pressing against it is the map refusing to show air.
    pub fn zoom_at(&mut self, anchor: Point, factor: f32) {
        if !(factor.is_finite() && factor > 0.0 && anchor.x.is_finite() && anchor.y.is_finite()) {
            return;
        }
        let scale = (self.k * factor).clamp(self.fill(), self.fill() * MAX_ZOOM);
        // the plate's own coordinates under the anchor, in degrees, which is the one
        // form that is independent of the scale being changed
        let lon = (anchor.x - self.x) / self.k;
        let lat = (self.y - anchor.y) / self.k;
        self.k = scale;
        self.x = anchor.x - lon * scale;
        self.y = anchor.y + lat * scale;
        self.settle();
    }

    /// Where a place is drawn, in view pixels.
    pub fn project(&self, at: Geo) -> Point {
        Point::new(self.x + at.lon * self.k, self.y - at.lat * self.k)
    }

    /// The place under a pixel: the inverse, and what a click needs to turn a point
    /// on screen into a coordinate.
    pub fn unproject(&self, at: Point) -> Geo {
        Geo::new((at.x - self.x) / self.k, (self.y - at.y) / self.k)
    }

    /// The zoom as a reader reads it: 1 is the whole plate in the box, 96 is the
    /// deepest level the outlines can answer.
    pub fn zoom(&self) -> f32 {
        self.k / self.fill()
    }

    /// Where the plate's centre currently sits in view pixels. A view needs it to
    /// compare two cameras without asking how big a degree is: `1` is the plate
    /// itself, shown, and the offset is where its middle would be drawn.
    pub fn offset(&self) -> Point {
        Point::new(self.x, self.y)
    }

    /// One eased step from here toward `target` — the straight line between two
    /// settled cameras.
    ///
    /// The line is why there is no clamp in here: "the plate covers the box" is
    /// `|x| ≤ 180k − w/2`, linear in `(x, y, k)`, so the set of cameras a gesture may
    /// hold is convex and the segment between two of its points is inside it. An
    /// eased frame therefore cannot flash air, and `target` must already be settled —
    /// a view that changed its box mid-glide calls [`Camera::resize`] first.
    pub fn toward(self, target: Camera, alpha: f32) -> Camera {
        let alpha = alpha.clamp(0.0, 1.0);
        Camera {
            w: target.w,
            h: target.h,
            x: self.x + (target.x - self.x) * alpha,
            y: self.y + (target.y - self.y) * alpha,
            k: self.k + (target.k - self.k) * alpha,
        }
    }

    /// The scale at which the plate covers the box on both axes, one of them exactly.
    fn fill(&self) -> f32 {
        (self.w / PLATE_W).max(self.h / PLATE_H)
    }

    /// Keep the plate over the box. Every gesture ends here, so a camera that shows
    /// air beside the world cannot be held: the scale stops at the fill, and each
    /// axis stops with its edge on the box's edge.
    fn settle(&mut self) {
        let fill = self.fill();
        self.k = if self.k.is_finite() { self.k.clamp(fill, fill * MAX_ZOOM) } else { fill };
        let slack_x = ((PLATE_W * self.k - self.w) / 2.0).max(0.0);
        let slack_y = ((PLATE_H * self.k - self.h) / 2.0).max(0.0);
        self.x = if self.x.is_finite() { self.x.clamp(-slack_x, slack_x) } else { 0.0 };
        self.y = if self.y.is_finite() { self.y.clamp(-slack_y, slack_y) } else { 0.0 };
    }
}

// --------------------------------------------------------- levels of detail

/// The box a ring occupies, in degrees. Computed once when the outlines are parsed,
/// so deciding whether one of fifteen hundred rings is on screen costs four
/// comparisons instead of a walk over its points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub west: f32,
    pub south: f32,
    pub east: f32,
    pub north: f32,
}

impl Bounds {
    /// The box of a ring, or `None` when it has no points at all.
    pub fn of(points: &[Geo]) -> Option<Bounds> {
        let first = points.first()?;
        let mut bounds = Bounds {
            west: first.lon,
            south: first.lat,
            east: first.lon,
            north: first.lat,
        };
        for point in points {
            bounds.west = bounds.west.min(point.lon);
            bounds.south = bounds.south.min(point.lat);
            bounds.east = bounds.east.max(point.lon);
            bounds.north = bounds.north.max(point.lat);
        }
        Some(bounds)
    }

    /// Whether a ring this size, here, is worth a stroke at this zoom: it reaches the
    /// box (a hairline of margin, so an edge that is half a pixel outside still
    /// paints its line) and it is at least a hairline across.
    ///
    /// At the opening zoom that drops the islets — sub-pixel coast is one grey step,
    /// and the pins are what names a place — while a continent, a country and an
    /// island of any consequence all pass. Deeper, the same test lets them in one by
    /// one, which is the whole of the level of detail.
    pub fn shows(&self, camera: &Camera) -> bool {
        let west = camera.x + self.west * camera.k;
        let east = camera.x + self.east * camera.k;
        let north = camera.y - self.north * camera.k;
        let south = camera.y - self.south * camera.k;
        let reaches = east >= -camera.w / 2.0 - HAIRLINE_PX
            && west <= camera.w / 2.0 + HAIRLINE_PX
            && south >= -camera.h / 2.0 - HAIRLINE_PX
            && north <= camera.h / 2.0 + HAIRLINE_PX;
        reaches && (east - west).max(south - north) >= HAIRLINE_PX
    }
}

/// Is `at` inside this ring? Ray casting east along the parallel: an odd number of
/// crossings is inside.
///
/// Geometry and nothing else — the same rule the module keeps for [`decimate`]: no
/// window type, no colour, no country. The ring is taken as the file writes it, closed
/// or not (every ring in the vendored file repeats its first point); the last vertex is
/// joined back to the first so an open ring still encloses what it draws.
///
/// Measured: 11.7 µs a frame, worst 63.7 µs, over the 91-cell zoom/pan grid of
/// `tests/map_country.rs` with the caller's boxes — this function is called only for the
/// rings whose box the point falls in, which at the world view is one to three of 1 632.
pub fn inside(ring: &[Geo], at: Geo) -> bool {
    // Two points are a line, and a line encloses nothing: a "hit" on one would be a hit on
    // a border the file drew twice.
    if ring.len() < 3 || !(at.lon.is_finite() && at.lat.is_finite()) {
        return false;
    }
    let mut crossings = 0usize;
    let mut before = ring.len() - 1;
    for index in 0..ring.len() {
        let (a, b) = (ring[index], ring[before]);
        // The half-open rule: an edge counts when the parallel separates its two ends, so
        // a vertex shared by two edges is counted once and a point on a horizontal edge is
        // not counted twice.
        if (a.lat > at.lat) != (b.lat > at.lat) {
            let cross = (b.lon - a.lon) * (at.lat - a.lat) / (b.lat - a.lat) + a.lon;
            if at.lon < cross {
                crossings += 1;
            }
        }
        before = index;
    }
    crossings % 2 == 1
}

/// The ring as a path a painter can stroke: projected, and thinned to what the
/// screen can show.
///
/// A Douglas–Peucker pass measured in pixels, not a stride: the 4 574-point ring in
/// the vendored file keeps its headlands at every zoom and loses the vertices that
/// land between two pixels. On the Overview's card that is 11 680 points drawn out of
/// the 92 142 in the rings the opening view shows — and 1 847 of 1 883 at the deepest
/// zoom, where every vertex is worth its pixel. Deeper zooms measure every deviation
/// larger and keep more of them, which is why this is a level of detail and not one
/// decimation frozen at load time.
///
/// It is per-frame work: the points depend on the ring and on the scale alone, never
/// on where the plate has been dragged, so a view that wants the frame back can cache
/// the kept vertices by ring and zoom.
///
/// A GeoJSON ring repeats its first point; both ends of the slice are kept, so the
/// closing vertex survives and a closed path still closes where it did.
pub fn decimate(points: &[Geo], camera: &Camera) -> Vec<Point> {
    let screen: Vec<Point> = points.iter().map(|at| camera.project(*at)).collect();
    if screen.len() <= 2 {
        return screen;
    }
    let mut keep = vec![false; screen.len()];
    keep[0] = true;
    keep[screen.len() - 1] = true;
    // an explicit stack, because a coastline is thousands of points long and the
    // worst case of a recursive split would be a frame per point
    let mut pending: Vec<(usize, usize)> = vec![(0, screen.len() - 1)];
    while let Some((first, last)) = pending.pop() {
        if last <= first + 1 {
            continue;
        }
        let mut worst = 0.0_f32;
        let mut at = first;
        for index in first + 1..last {
            let off = off_segment(screen[index], screen[first], screen[last]);
            if off > worst {
                worst = off;
                at = index;
            }
        }
        if worst > TOLERANCE_PX {
            keep[at] = true;
            pending.push((first, at));
            pending.push((at, last));
        }
    }
    screen
        .into_iter()
        .zip(keep)
        .filter(|(_, kept)| *kept)
        .map(|(point, _)| point)
        .collect()
}

/// How far `point` sits off the segment `from`–`to`, in the segment's own units.
fn off_segment(point: Point, from: Point, to: Point) -> f32 {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = dx * dx + dy * dy;
    if length <= f32::MIN_POSITIVE {
        return ((point.x - from.x).powi(2) + (point.y - from.y).powi(2)).sqrt();
    }
    let t = (((point.x - from.x) * dx + (point.y - from.y) * dy) / length).clamp(0.0, 1.0);
    let (x, y) = (from.x + t * dx, from.y + t * dy);
    ((point.x - x).powi(2) + (point.y - y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    /// The box most of these tests are drawn in: the Overview's map card, 1 000 px
    /// wide and 470 tall — wider than the plate's 2:1, so at the opening zoom the
    /// width is exactly filled and the height has a sliver to spare.
    const CARD: (f32, f32) = (1000.0, 470.0);

    /// The plate covers the box, with no air on any side.
    fn covers(camera: &Camera, size: (f32, f32)) -> bool {
        let north_west = camera.project(Geo::new(-PLATE_W / 2.0, PLATE_H / 2.0));
        let south_east = camera.project(Geo::new(PLATE_W / 2.0, -PLATE_H / 2.0));
        north_west.x <= -size.0 / 2.0 + 1e-3
            && north_west.y <= -size.1 / 2.0 + 1e-3
            && south_east.x >= size.0 / 2.0 - 1e-3
            && south_east.y >= size.1 / 2.0 - 1e-3
    }

    /// How far a point sits off the nearest segment of a path.
    fn off_path(point: Point, path: &[Point]) -> f32 {
        path.windows(2)
            .map(|pair| off_segment(point, pair[0], pair[1]))
            .fold(f32::INFINITY, f32::min)
    }

    // ------------------------------------------------------------- projection

    #[test]
    fn a_place_and_its_pixel_are_each_other_s_inverse() {
        let camera = Camera::new(CARD);
        let mut worst = 0.0_f32;
        let mut lon = -180.0;
        while lon <= 180.0 {
            let mut lat = -90.0;
            while lat <= 90.0 {
                let there = camera.unproject(camera.project(Geo::new(lon, lat)));
                worst = worst.max((there.lon - lon).abs()).max((there.lat - lat).abs());
                lat += 7.5;
            }
            lon += 11.25;
        }
        // 1e-4° is 11 m: a click cannot be placed more precisely than its pixel
        assert!(worst < 1e-4, "the round trip loses {worst} degrees");
        let berlin = camera.unproject(camera.project(Geo::new(13.404_954, 52.520_008)));
        assert!(close(berlin.lon, 13.404_954, 1e-3) && close(berlin.lat, 52.520_008, 1e-3));
    }

    #[test]
    fn the_plate_runs_east_and_down_from_its_centre() {
        let camera = Camera::new(CARD);
        let centre = camera.project(Geo::new(0.0, 0.0));
        // the plate's centre is the box's centre, at every zoom
        assert!(close(centre.x, 0.0, 1e-4) && close(centre.y, 0.0, 1e-4), "{centre:?}");
        // east is right and north is up, which is the one sign flip in this file
        assert!(camera.project(Geo::new(10.0, 0.0)).x > centre.x);
        assert!(camera.project(Geo::new(0.0, 10.0)).y < centre.y);
        // and a degree is the same length on both axes: the plate keeps its shape
        let across = camera.project(Geo::new(1.0, 0.0)).x - centre.x;
        let up = centre.y - camera.project(Geo::new(0.0, 1.0)).y;
        assert!(close(across, up, 1e-4), "{across} px east against {up} px north");
    }

    // ----------------------------------------------------------------- camera

    #[test]
    fn a_new_camera_fills_the_box_on_both_axes() {
        for size in [CARD, (400.0, 400.0), (1440.0, 900.0), (900.0, 300.0), (1.0, 1.0)] {
            let camera = Camera::new(size);
            assert!(covers(&camera, size), "{size:?} is not filled");
            assert!(close(camera.zoom(), 1.0, 1e-6), "{size:?} opens at zoom {}", camera.zoom());
        }
        // the tighter axis is flush, not merely covered
        let camera = Camera::new(CARD);
        let north_west = camera.project(Geo::new(-PLATE_W / 2.0, PLATE_H / 2.0));
        assert!(close(north_west.x, -CARD.0 / 2.0, 1e-3), "the west edge is at {}", north_west.x);
    }

    #[test]
    fn zooming_keeps_the_place_under_the_pointer() {
        for anchor in [Point::new(-320.0, 90.0), Point::new(0.0, 0.0), Point::new(410.0, -180.0)] {
            for factor in [1.6_f32, 0.5, 3.0, 0.75, 1.0] {
                let mut camera = Camera::new(CARD);
                // room to move in both directions, away from either end of the clamp
                camera.zoom_at(Point::new(0.0, 0.0), 8.0);
                let before = camera.unproject(anchor);
                camera.zoom_at(anchor, factor);
                let after = camera.unproject(anchor);
                let lost = (after.lon - before.lon).abs().max((after.lat - before.lat).abs());
                assert!(camera.zoom() > 1.0, "the clamp was reached at {factor}: {}", camera.zoom());
                assert!(lost < 1e-3, "×{factor} at {anchor:?} moved the anchor {lost} degrees");
            }
        }
    }

    #[test]
    fn the_zoom_stops_at_the_whole_plate_and_at_the_deepest_level() {
        let mut camera = Camera::new(CARD);
        for _ in 0..40 {
            camera.zoom_at(Point::new(300.0, 100.0), 0.5);
        }
        assert!(close(camera.zoom(), 1.0, 1e-4), "zoom out stops at the plate: {}", camera.zoom());
        assert!(covers(&camera, CARD));

        for _ in 0..80 {
            camera.zoom_at(Point::new(300.0, 100.0), 2.0);
        }
        assert!(close(camera.zoom(), MAX_ZOOM, 1e-3), "zoom in stops at {}", camera.zoom());
        assert!(covers(&camera, CARD));
        // at the deep end there is slack on both axes, so the anchor still holds
        let anchor = Point::new(120.0, -60.0);
        let before = camera.unproject(anchor);
        camera.zoom_at(anchor, 2.0);
        let after = camera.unproject(anchor);
        assert!((after.lon - before.lon).abs() < 1e-3, "{after:?} against {before:?}");
    }

    #[test]
    fn the_world_cannot_be_dragged_out_of_the_plate() {
        // dragged as far as it goes at the opening zoom: the plate's corner meets
        // the box's corner, and no further
        let mut camera = Camera::new(CARD);
        camera.pan((5000.0, 5000.0));
        let corner = camera.project(Geo::new(-PLATE_W / 2.0, PLATE_H / 2.0));
        assert!(close(corner.x, -CARD.0 / 2.0, 1e-3), "west edge at {}", corner.x);
        assert!(corner.y <= -CARD.1 / 2.0 + 1e-3, "north edge at {}", corner.y);

        // zoomed in, the same drag ends with the south-east corner on the box
        let mut camera = Camera::new(CARD);
        camera.zoom_at(Point::new(0.0, 0.0), 4.0);
        camera.pan((-1.0e6, -1.0e6));
        let corner = camera.project(Geo::new(PLATE_W / 2.0, -PLATE_H / 2.0));
        assert!(close(corner.x, CARD.0 / 2.0, 1e-3), "east edge at {}", corner.x);
        assert!(close(corner.y, CARD.1 / 2.0, 1e-3), "south edge at {}", corner.y);
        // and nothing outside the plate is ever under the box
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
        // the frame loop: drag, zoom about the pointer, occasionally resize. Four
        // hundred frames of the worst case, with the invariant checked every one.
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
            assert!(camera.zoom() >= 1.0 && camera.zoom() <= MAX_ZOOM + 1e-3);
        }
    }

    #[test]
    fn a_camera_eased_between_two_settled_ones_is_still_settled() {
        // The view eases x, y and k toward a target, so every frame draws a value
        // *between* two settled cameras. If that value could sit outside the box, a
        // gesture would flash air on its way. It cannot: "the plate covers the box"
        // is |x| ≤ 180k − w/2 — linear in (x, k) — so the settled set is convex and
        // the segment between two of its points is inside it. No second clamp.
        let mut from = Camera::new(CARD);
        from.zoom_at(Point::new(-480.0, -220.0), 3.0);
        from.pan((900.0, 400.0));
        let mut to = Camera::new(CARD);
        to.zoom_at(Point::new(470.0, 210.0), 12.0);
        to.pan((-1.0e5, 1.0e5));
        for step in 0..=20 {
            let t = step as f32 / 20.0;
            let between = from.toward(to, t);
            assert!(covers(&between, CARD), "step {step} of the ease leaves air");
            // and the ends are the ends: alpha 0 is this camera, alpha 1 the target
            if step == 0 {
                assert_eq!(between, from);
            }
            if step == 20 {
                assert_eq!(between, to);
            }
        }
    }

    #[test]
    fn the_offset_is_the_plate_s_middle_and_the_zoom_names_the_fit() {
        let camera = Camera::new(CARD);
        assert_eq!(camera.offset(), Point::new(0.0, 0.0));
        assert_eq!(camera.offset(), camera.project(Geo::new(0.0, 0.0)));
        // the card's width is exactly the plate's at the fit, so a drag cannot move
        // it sideways at all; the height has 15 px to spare and takes 12 of them
        let mut flush = camera;
        flush.pan((-40.0, 12.0));
        assert_eq!(flush.offset(), Point::new(0.0, 12.0), "the width has no slack");
        // zoomed in, both axes have somewhere to go and keep what they are given
        let mut deeper = camera;
        deeper.zoom_at(Point::new(0.0, 0.0), 4.0);
        deeper.pan((-40.0, 12.0));
        assert_eq!(deeper.offset(), Point::new(-40.0, 12.0));
        // the fit is what the zoom is measured against, so filling the box is ×1
        assert!(close(deeper.zoom(), 4.0, 1e-4));
    }

    #[test]
    fn resizing_seats_the_plate_in_the_new_box() {
        let mut camera = Camera::new(CARD);
        camera.zoom_at(Point::new(0.0, 0.0), 6.0);
        let centre = camera.unproject(Point::new(0.0, 0.0));
        // a window dragged narrower keeps the place in the middle of the card
        let small = (620.0, 300.0);
        camera.resize(small);
        assert!(covers(&camera, small));
        let after = camera.unproject(Point::new(0.0, 0.0));
        assert!(close(after.lon, centre.lon, 1e-3) && close(after.lat, centre.lat, 1e-3));
        // and a window grown past the plate's old fill raises the scale with it
        let mut camera = Camera::new((400.0, 200.0));
        camera.resize((1400.0, 800.0));
        assert!(covers(&camera, (1400.0, 800.0)), "the plate did not grow with the box");
        assert!(close(camera.zoom(), 1.0, 1e-4));
    }

    // -------------------------------------------------------- levels of detail

    #[test]
    fn a_ring_is_drawn_when_it_reaches_the_box_and_is_wide_enough() {
        // Europe, at the opening zoom: 1 632 rings in the vendored file, and this is
        // one that has to survive
        let europe = Bounds { west: -10.0, south: 36.0, east: 30.0, north: 60.0 };
        let camera = Camera::new(CARD);
        assert!(europe.shows(&camera));

        // an islet 0.04° across is a tenth of a pixel on a 1 000 px card: nothing to
        // draw, and nothing to see if it were drawn
        let islet = Bounds { west: 14.30, south: 35.90, east: 14.34, north: 35.93 };
        assert!(!islet.shows(&camera), "0.04° is {} px at the opening zoom", 0.04 * camera.k);
        // zoomed to the same place, the same islet is three pixels of coast
        let mut deep = camera;
        deep.zoom_at(camera.project(Geo::new(14.32, 35.91)), 32.0);
        assert!(islet.shows(&deep));

        // at 32× the box holds 11° of longitude, so a continent still reaches it —
        // part of a ring in view is the whole ring's reason to be drawn
        assert!(europe.shows(&deep), "Europe is in view at {}×", deep.zoom());
        // but one on the far side of the plate is not drawn however big it is
        let pacific = Bounds { west: 120.0, south: -10.0, east: 160.0, north: 10.0 };
        assert!((pacific.east - pacific.west) * deep.k > deep.w / 2.0, "the ring is huge");
        assert!(!pacific.shows(&deep), "a ring off the box at {}×", deep.zoom());
    }

    #[test]
    fn a_ring_just_outside_the_edge_still_paints_its_hairline() {
        let camera = Camera::new(CARD);
        // the box's east edge is at 500 px; a ring whose west edge is at 500.4 px
        // strokes a line into it and must not be culled
        let lon = camera.unproject(Point::new(500.4, 0.0)).lon;
        let ring = Bounds { west: lon, south: -1.0, east: lon + 0.01, north: 1.0 };
        assert!(ring.shows(&camera));
        let far = Bounds { west: lon + 1.0, south: -1.0, east: lon + 2.0, north: 1.0 };
        assert!(!far.shows(&camera));
    }

    #[test]
    fn bounds_are_the_box_of_the_ring_and_nothing_for_no_ring() {
        let ring = [Geo::new(4.0, 50.0), Geo::new(6.0, 47.0), Geo::new(2.0, 52.0)];
        let bounds = Bounds::of(&ring).expect("three points have a box");
        assert_eq!(bounds, Bounds { west: 2.0, south: 47.0, east: 6.0, north: 52.0 });
        assert_eq!(Bounds::of(&[]), None);
        // a bag of degrees is a box too, and a single point is a box of no width
        let one = Bounds::of(&[Geo::new(13.4, 52.5)]).expect("one point has a box");
        assert_eq!(one, Bounds { west: 13.4, south: 52.5, east: 13.4, north: 52.5 });
    }

    /// A coastline that wiggles: `count` vertices, `wiggle` degrees of headland.
    fn coast(count: usize, wiggle: f32) -> Vec<Geo> {
        (0..count)
            .map(|index| {
                let t = index as f32 / (count - 1) as f32;
                Geo::new(-10.0 + 20.0 * t, 40.0 + wiggle * (t * 37.0).sin() * (t * 11.0).cos())
            })
            .collect()
    }

    #[test]
    fn a_straight_coast_costs_two_points() {
        let camera = Camera::new(CARD);
        let straight: Vec<Geo> = (0..200).map(|i| Geo::new(-10.0 + 0.1 * i as f32, 40.0)).collect();
        let path = decimate(&straight, &camera);
        assert_eq!(path.len(), 2, "a straight line needs its two ends and nothing else");
        // a two-point ring is passed through rather than thinned
        let pair = decimate(&[Geo::new(0.0, 0.0), Geo::new(1.0, 1.0)], &camera);
        assert_eq!(pair.len(), 2);
        assert!(decimate(&[Geo::new(0.0, 0.0)], &camera).len() == 1);
    }

    #[test]
    fn a_headland_survives_at_every_zoom() {
        for factor in [1.0_f32, 4.0, 16.0, MAX_ZOOM] {
            let mut camera = Camera::new(CARD);
            camera.zoom_at(Point::new(0.0, 0.0), factor);
            // three points, the middle one a long way off the line between the ends
            let spit = [Geo::new(-1.0, 40.0), Geo::new(0.0, 41.0), Geo::new(1.0, 40.0)];
            let path = decimate(&spit, &camera);
            assert_eq!(path.len(), 3, "×{factor} dropped the headland");
            // and it is where it belongs: the middle vertex is the top of the spit
            assert!(path[1].y < path[0].y && path[1].y < path[2].y);
        }
    }

    #[test]
    fn the_thinned_coast_stays_within_a_pixel_of_the_original() {
        let camera = Camera::new(CARD);
        for factor in [1.0_f32, 3.0, 12.0] {
            let mut camera = camera;
            camera.zoom_at(Point::new(0.0, 0.0), factor);
            let ring = coast(400, 0.6);
            let path = decimate(&ring, &camera);
            assert!(path.len() >= 2 && path.len() < ring.len(), "×{factor} kept {}", path.len());
            // every vertex of the original is within the tolerance of what replaces it
            let mut worst = 0.0_f32;
            for at in &ring {
                worst = worst.max(off_path(camera.project(*at), &path));
            }
            assert!(
                worst <= TOLERANCE_PX + 1e-3,
                "×{factor}: the coast moved {worst} px, {} of {} points kept",
                path.len(),
                ring.len()
            );
        }
    }

    #[test]
    fn zooming_in_never_loses_a_vertex() {
        let ring = coast(300, 0.4);
        let mut camera = Camera::new(CARD);
        let mut previous = 0;
        for step in 0..12 {
            if step > 0 {
                camera.zoom_at(Point::new(0.0, 0.0), 1.7);
            }
            let kept = decimate(&ring, &camera).len();
            assert!(
                kept >= previous,
                "step {step} at {}× kept {kept} after {previous}",
                camera.zoom()
            );
            previous = kept;
        }
        // at the deepest level every vertex is worth its pixel
        assert!(previous > decimate(&ring, &Camera::new(CARD)).len());
    }

    #[test]
    fn a_closed_ring_keeps_the_vertex_it_closes_on() {
        let camera = Camera::new(CARD);
        // a GeoJSON ring repeats its first point
        let mut ring = coast(120, 0.5);
        let first = ring[0];
        ring.push(first);
        let path = decimate(&ring, &camera);
        assert_eq!(path[0], path[path.len() - 1], "the ring no longer closes on itself");
        assert_eq!(path[0], camera.project(first));
        assert!(path.len() >= 3, "a closed path needs three points to stroke");
    }
}
