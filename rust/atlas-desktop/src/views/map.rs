//! The world map: a plate of coastlines from the vendored Natural Earth outlines,
//! filled land, a graticule every 30°, and one dot per group of places the reporting
//! comes from.
//!
//! Projection, scale, bounds and the level of detail are [`crate::geo`]'s, not a second
//! camera of this file's own: `geo::Camera` keeps the place under the pointer under the
//! pointer and culls by ring bounds. It also refuses to letterbox the box it is given (its
//! floor is the zoom that fills that box), which is why the view hands it the world's own 2:1
//! box inside the card ([`world_box`]): the plate then always shows all 360 × 180 degrees, and
//! the air the card has beyond that aspect is page beside the plate rather than world cropped
//! off it. This file is the *view*: it accumulates a gesture, eases toward it, and paints what
//! `geo` says a camera can see.
//!
//! Nothing on the plate is ever missing for the sake of the frame budget. The budget
//! decides how *detailed* the coast is — the three tiers below — never which parts of the
//! world exist, so a zoom changes the drawing's resolution and not its contents. (The old
//! loop stopped drawing rings once the budget ran out, so coastlines appeared and
//! disappeared mid-gesture; that is the "it bugs out" a reader reported.)
//!
//! The fill is even-odd and its spans are one pixel tall. It covers every ring the coast
//! strokes — a ring stroked and not filled is land drawn as a hole — so it is bounded by the
//! coast's own segment budget rather than by a second cap of its own; the table on [`TIERS`]
//! carries the numbers the tiers were chosen from.
//!
//! Monochrome by construction: every colour is `theme::*` and every stroke is 1 px.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use egui::{Color32, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::nf;
use crate::data::Pin;
use crate::geo::{decimate, Bounds, Camera, Geo, Point};
use crate::gesture::{Gesture, PINCH_EVENT_MAX, ZOOM_PENDING_MAX};
use crate::theme;

/// The graticule: one line every 30° of longitude and of latitude.
pub const GRATICULE_STEP: f32 = 30.0;

/// Log-zoom per scrolled point of the plate's own wheel: 14 points — one notch of a mouse
/// wheel — is `e^0.031`, 3.1%. This is the number the plate shipped with, and it is kept
/// because it is the *finger memory* of the map rather than a rule of the accumulator: it
/// decides how much a swipe banks, so it decides whether a fast two-finger scroll reaches the
/// deepest zoom by the time it stops. Measured in `tests/frame_cost.rs`: at 0.0022 the
/// 40-step sweep of 170 pt per frame peaks at ×93.5 and comes back to ×1.0; at the graph's
/// gentler 0.0015 the same sweep tops out at ×89.3, because the frame's rate spends each
/// event as fast as it arrives and the queue never holds a surplus.
///
/// It is not a second copy of `gesture::WHEEL_GAIN`: that one is the accumulator's own
/// reading for a wheel it is handed raw, and the two differ on purpose — a graph of nodes
/// wants a gentler notch than a plate of coastlines.
pub const WHEEL_GAIN: f32 = 0.0022;

/// How far apart two drawn dots must be before they are two dots, in pixels. This is the
/// whole anti-blob rule: clustering runs on screen distance, so it holds at every zoom
/// instead of collapsing by a fixed degree count.
pub const CLUSTER_PX: f32 = 15.0;

/// The most zoom one frame may apply, in log units (≈ 1.4× per frame). A pinch that
/// arrives faster than the display can show it is spread over frames, not jumped.
pub const ZOOM_STEP_MAX: f32 = 0.35;

/// The budget that decides the *tier*, in segments: below it the frame is drawn at the
/// finest simplification, above it every visible ring is coarsened one tier.
///
/// It is not a filter. A ring is never dropped for being unaffordable — a coastline that
/// vanishes when the plate moves is a bug, not a budget.
pub const MAX_SEGMENTS: usize = 24_000;

/// The three simplification tiers, as a screen-space tolerance in pixels.
///
/// Tier 0 is [`decimate`], the file's own level of detail: a Douglas–Peucker that measures
/// half a pixel, the width of the hairline the coast is stroked with. Tiers 1 and 2 are one
/// linear distance pass each ([`apart`]), because a Douglas–Peucker over the whole file
/// measured 58.9 ms at the world view — the one thing a frame cannot spend.
///
/// Measured on the vendored outlines in the 1 000 × 470 card, one frame per row, over two
/// runs (the spread is the machine, not the code), printed by
/// `the_parse_time_and_the_frame_line_count_are_measured` and the zoom sweep, so the table
/// can be checked rather than trusted:
///
/// | zoom  | visible points | tier | segments | release   | debug  |
/// |-------|----------------|------|----------|-----------|--------|
/// | 1×    | 99 613         | 1    | 13 972   | 0.2–0.4 ms | 1.2–1.7 ms |
/// | 2.5×  | ~68 000        | 1    | 17 110   | 0.2 ms    | 0.8 ms |
/// | 6.2×  | ~30 000        | 1    | 10 413   | 0.03 ms   | 0.4 ms |
/// | 15.5× | ~4 000         | 0    | 2 488    | 0.07 ms   | 1.2 ms |
/// | 38.5× | ~1 200         | 0    | 1 141    | 0.03 ms   | 0.4 ms |
/// | 96×   | ~1 100         | 0    | 1 057    | 0.02 ms   | 0.4 ms |
///
/// The shape is the point: the tier is a function of the zoom and of nothing else, and the
/// set of rings drawn is a function of the camera alone, so the same coast is drawn at every
/// step of a gesture — in more or less detail, never in or out.
pub const TIERS: [f32; 3] = [0.5, 1.5, 4.0];

/// The land's grey: `theme::RAISED` (24), one flat step above the page (10) and well below
/// the hairline (36).
///
/// Chosen from the palette rather than mixed here: 14 levels above the page is a fill a
/// reader can see as land without it becoming a surface they might try to click, and 12
/// levels under `HAIRLINE` keeps every line on the plate — coast, graticule, axes — darker
/// than the area it crosses. Not `PANEL` (16, six levels: too faint to read as land) and
/// not `HAIRLINE_SOFT` (28, a line's grey used as an area).
pub const LAND: Color32 = theme::RAISED;

/// A longitude jump wider than this is the antimeridian, not a coastline.
const ANTIMERIDIAN: f32 = 180.0;

/// How tall one horizontal span of the land fill is, in pixels. One pixel is the resolution
/// of the stair-step left along a shallow coast; the coastline's own hairline is drawn over
/// it, which is why it is not worth a finer sweep.
const FILL_ROW_PX: f32 = 1.0;

// ------------------------------------------------------------------ the world

/// One ring of the file: a piece of a coastline that never crosses the antimeridian.
struct Ring {
    points: Vec<Geo>,
    /// The box it occupies, computed once at parse time: four comparisons decide whether
    /// this ring can appear on the plate at all — and which few rings a click has to test.
    bounds: Bounds,
    /// The country this coast belongs to, by ISO-3166-1 **alpha-3**: the archive's own
    /// alphabet (`articles.iso` holds `USA`, `CHN`, `DEU`, and `Archive::country_stories`
    /// reads that column). Empty when the file names none the archive could answer for.
    ///
    /// Read from `properties` at parse time and never again: a ring is a piece of one
    /// country, and the file's `ISO_A2` cannot be used — it is `-99` for eight features,
    /// France and Norway among them. `ISO_A3_EH`, then `ISO_A3`, then `ADM0_A3`, which
    /// every feature has.
    iso: Box<str>,
}

/// The world's outlines, loaded once and painted every frame.
pub struct World {
    /// Biggest first: a budget should thin the islets, not the continents.
    rings: Vec<Ring>,
    /// Points kept in `rings`, before any screen-space simplification.
    points: usize,
    /// One point per country, where a cartographer would write its name: the file's own
    /// `LABEL_X`/`LABEL_Y`, not a centroid computed here.
    ///
    /// This is the second position a story may be drawn at, and the justification is the
    /// file's: a Natural Earth label point is a *cartographic* point chosen to sit inside the
    /// country, on land, away from its neighbours — not a city and not the average of a
    /// coastline that would land in the sea. A story filed in Kyiv that also names Germany is
    /// one story with one coordinate (the city); its German mark is at Germany's label point,
    /// and it is drawn as a country ring, not as a second story dot, so the two can never be
    /// mistaken for two reports from two places.
    ///
    /// Keyed by ISO-3166-1 alpha-3, the archive's alphabet; a feature the file gives no label
    /// point or no code simply has no mark. One entry per code, the largest feature kept (the
    /// shipped file has one feature per code, so the rule never decides anything there).
    labels: HashMap<Box<str>, Geo>,
    /// How long the file took to read and parse, kept for the card's note.
    pub parse_ms: f64,
}

impl World {
    /// An empty world: no outlines, but the graticule and the pins still draw.
    pub fn empty() -> World {
        World { rings: Vec::new(), points: 0, labels: HashMap::new(), parse_ms: 0.0 }
    }

    /// Parse the vendored `ne_50m_countries.geojson`. A missing or unreadable file is an
    /// empty world, never a crash.
    pub fn load(path: &Path) -> World {
        let started = Instant::now();
        let Ok(text) = std::fs::read_to_string(path) else {
            return World::empty();
        };
        let mut world = World::from_geojson(&text);
        world.parse_ms = started.elapsed().as_secs_f64() * 1000.0;
        world
    }

    /// Parse GeoJSON text: every `Polygon` and `MultiPolygon` ring, at full resolution and
    /// split where it steps over the antimeridian.
    pub fn from_geojson(text: &str) -> World {
        let root: serde_json::Value = match serde_json::from_str(text) {
            Ok(root) => root,
            Err(_) => return World::empty(),
        };
        let Some(features) = root.get("features").and_then(|value| value.as_array()) else {
            return World::empty();
        };
        let mut rings: Vec<Ring> = Vec::new();
        // One label point per country, the largest feature's kept. `first` is where this
        // feature's rings start, so the feature's own extent is read off the bounds already
        // computed for them — no second pass over 99 613 coordinates.
        let mut labels: HashMap<Box<str>, (f32, Geo)> = HashMap::new();
        for feature in features {
            let Some(geometry) = feature.get("geometry") else { continue };
            let kind = geometry.get("type").and_then(|value| value.as_str()).unwrap_or("");
            let Some(coordinates) = geometry.get("coordinates") else { continue };
            let properties = feature.get("properties").unwrap_or(&serde_json::Value::Null);
            // The country the ring belongs to, read once per feature: this is what makes a
            // click on land a click on a country, and it costs one property read per feature.
            let iso = code_of(properties);
            let first = rings.len();
            match kind {
                "Polygon" => push_polygon(coordinates, &iso, &mut rings),
                "MultiPolygon" => {
                    if let Some(polygons) = coordinates.as_array() {
                        for polygon in polygons {
                            push_polygon(polygon, &iso, &mut rings);
                        }
                    }
                }
                _ => {}
            }
            let Some(at) = label_of(properties) else { continue };
            if iso.is_empty() {
                continue;
            }
            let extent = feature_extent(&rings[first..]);
            let slot = labels.entry(iso.as_str().into()).or_insert((extent, at));
            if extent > slot.0 {
                *slot = (extent, at);
            }
        }
        // biggest first, so a budget is spent on the continents
        let extent = |ring: &Ring| -> f32 {
            (ring.bounds.east - ring.bounds.west).max(0.0)
                * (ring.bounds.north - ring.bounds.south).max(0.0)
        };
        let mut keyed: Vec<(f32, Ring)> =
            rings.into_iter().map(|ring| (extent(&ring), ring)).collect();
        keyed.sort_by(|left, right| right.0.total_cmp(&left.0));
        let rings: Vec<Ring> = keyed.into_iter().map(|(_, ring)| ring).collect();
        let points = rings.iter().map(|ring| ring.points.len()).sum();
        let labels: HashMap<Box<str>, Geo> =
            labels.into_iter().map(|(iso, (_, at))| (iso, at)).collect();
        World { rings, points, labels, parse_ms: 0.0 }
    }

    /// How many coordinates the file gave (all of them are kept).
    pub fn points(&self) -> usize {
        self.points
    }

    /// How many coastlines the file gave, after the antimeridian split.
    pub fn lines(&self) -> usize {
        self.rings.len()
    }

    /// Every ring a camera can see, as paths of screen points: the frame's geometry.
    ///
    /// One tier, chosen from the work the frame can afford, and the *same* set of rings at
    /// every tier: a budget may change the detail and never the contents. The count is
    /// taken in `Point` space, before the plate's offset is added, so a frame that lands on
    /// a coarse tier never pays for the fine paths it did not keep.
    pub fn frame(&self, camera: &Camera, centre: Pos2) -> Plate {
        let visible: Vec<&Ring> = self
            .rings
            .iter()
            .filter(|ring| ring.bounds.shows(camera))
            .collect();
        let points: usize = visible.iter().map(|ring| ring.points.len()).sum();
        // The finest tier is [`decimate`] over every point of every visible ring: 99 613
        // Douglas–Peucker candidates at the world view, measured at 58.9 ms — a frame that
        // can see the whole file starts one tier coarser instead.
        let mut tier = if points <= FINEST_POINTS { 0 } else { 1 };
        let mut paths = self.tier_paths(&visible, camera, tier);
        let mut segments = segments_of(&paths);
        while segments > MAX_SEGMENTS && tier + 1 < TIERS.len() {
            tier += 1;
            paths = self.tier_paths(&visible, camera, tier);
            segments = segments_of(&paths);
        }
        let paths: Vec<Vec<Pos2>> = paths.iter().map(|path| placed(path, centre)).collect();
        // The fill covers **every ring the coast strokes**, from the very vertices the coast is
        // stroked from — no second decimation and no second budget.
        //
        // It used to stop at a 12 000-vertex cap, biggest ring first, and that cap could only
        // ever buy speed by drawing land as a hole: measured on the shipped file at ×1 over
        // (10° E, 50° N) the coast stroked 835 rings and the fill covered 163 of them, leaving
        // 672 outlines around page grey, 38 of them at least 4 × 4 px and the widest 20.6 px;
        // at ×3 it was 230 of 318, widest 31.5 px. A reader sees exactly that as borders that
        // are not completely there.
        //
        // The cost is the coast's own, because a path's vertices are its segments plus one and
        // the tier policy above already bounds the segments. Filling every ring takes the world
        // view from 11 996 to 15 343 fill vertices (+28 %) and the mesh from 11.0 to 10.5 ms
        // (best of twelve each, the load moving more than the code: the added rings are tiny and
        // the sweep is per row). From ×12 on nothing changes — every ring already fitted.
        let filled = paths.len();
        Plate { paths, filled, segments, tier }
    }

    /// The country under a place, by ISO-3166-1 alpha-3 — the archive's own code — or `None`
    /// for water.
    ///
    /// The ring's own box first: four comparisons throw out every coast the point cannot be
    /// in, which at the world view leaves one to three rings of 1 632 and at a city zoom a
    /// handful. Then the ray cast ([`geo::inside`]). Measured over the 91-cell grid of
    /// `tests/map_country.rs`: median 11.7 µs, worst 63.7 µs a call, with these boxes
    /// precomputed at parse time — recomputing them per call cost 1 122.7 µs, which is the
    /// whole reason they are kept on the ring.
    ///
    /// Boxes-first is also the order the plate draws in (biggest ring first), so a point in a
    /// lake answers the country around it — the ring the fill leaves as page grey is still
    /// the country a reader clicked.
    pub fn country_at(&self, at: Geo) -> Option<&str> {
        if !(at.lon.is_finite() && at.lat.is_finite()) {
            return None;
        }
        self.rings
            .iter()
            .find(|ring| {
                !ring.iso.is_empty()
                    && at.lon >= ring.bounds.west
                    && at.lon <= ring.bounds.east
                    && at.lat >= ring.bounds.south
                    && at.lat <= ring.bounds.north
                    && crate::geo::inside(&ring.points, at)
            })
            .map(|ring| &*ring.iso)
    }

    /// The country under a pixel of the plate: the camera's inverse, then [`World::country_at`].
    /// `screen` is measured from the plate's centre, the way the camera projects.
    pub fn country_at_plate(&self, camera: &Camera, screen: Point) -> Option<&str> {
        self.country_at(camera.unproject(screen))
    }

    /// Where the file puts this country's name, in degrees — the point a [`CountryMark`] is
    /// drawn at, or `None` for a code the file does not draw or gives no label point.
    ///
    /// The label point, not a centroid of the rings: a centroid of an archipelago lands in the
    /// sea, and a centroid of a country with an overseas department lands in neither. The
    /// file's own point is inside the country by construction, which is the property a mark
    /// that stands for the country needs.
    pub fn label_point(&self, iso: &str) -> Option<Geo> {
        self.labels.get(iso).copied()
    }

    /// One tier's paths, before they are placed on the plate.
    ///
    /// Tier 0 is [`decimate`], the file's own level of detail: a Douglas–Peucker over every
    /// point, which is the finest a screen can be given. The coarser tiers are a single
    /// linear pass at their own tolerance, because a Douglas–Peucker over 99 613 points
    /// measured 58.9 ms at the world view — the one thing this frame cannot spend.
    fn tier_paths(&self, visible: &[&Ring], camera: &Camera, tier: usize) -> Vec<Vec<Point>> {
        visible
            .iter()
            .map(|ring| {
                let thinned = if tier == 0 {
                    decimate(&ring.points, camera)
                } else {
                    apart(&ring.points, camera, TIERS[tier])
                };
                outline_of(&ring.points, camera, thinned)
            })
            .collect()
    }
}

/// How many points of the visible rings a frame may hand to [`decimate`] before the finest
/// tier is off the table.
///
/// Above this a frame starts one tier coarser, which is a linear pass. Measured: a
/// Douglas–Peucker over 11 000 points took 3.8 ms, over 4 000 about 1.2 ms — and above 4 000
/// points the file's own resolution is already under the pixel a hairline occupies.
const FINEST_POINTS: usize = 4_000;

fn segments_of(paths: &[Vec<Point>]) -> usize {
    paths.iter().map(|path| path.len().saturating_sub(1)).sum()
}

/// One frame's worth of screen geometry, plus the numbers the tests print.
pub struct Plate {
    /// Every ring on the plate, biggest first: what the coast is stroked from.
    pub paths: Vec<Vec<Pos2>>,
    /// How many of the first paths the fill covers: all of them, since a ring stroked and
    /// not filled is land drawn as a hole. Reported because `fill()` is a prefix and the
    /// tests read the two numbers against each other.
    pub filled: usize,
    /// Segments stroked this frame: `Σ(points − 1)` over `paths`.
    pub segments: usize,
    /// The simplification tier this frame is drawn at: 0 is the finest.
    pub tier: usize,
}

impl Plate {
    /// The rings the fill covers: a prefix of `paths`, because the world is sorted biggest
    /// first, and today the whole of it.
    pub fn fill(&self) -> &[Vec<Pos2>] {
        &self.paths[..self.filled.min(self.paths.len())]
    }
}

/// A ring's screen path in the plate's own pixels.
fn placed(path: &[Point], centre: Pos2) -> Vec<Pos2> {
    path.iter()
        .map(|point| Pos2::new(centre.x + point.x, centre.y + point.y))
        .collect()
}

/// A ring projected to the plate and thinned by distance in one linear pass.
///
/// This is the coarser tiers' simplifier, and it is what makes them affordable: a vertex
/// within `tolerance` of the vertex kept before it is dropped, so what is drawn is never
/// further than `tolerance` from what the file says (the dropped point's distance to a
/// *kept* point bounds its distance to the line through it) and the pass costs one
/// comparison per coordinate instead of a Douglas–Peucker over all of them.
///
/// A **needle** — three kept vertices whose first and last are within one and a half
/// tolerances of each other — is removed as the pass goes, and it is not cosmetic. Drawn as
/// two nearly coincident legs, a needle is what makes a ring cross itself, and the fill is
/// even-odd: a ring that crosses itself paints a *hole in the land* where the little loop
/// closes. Measured on the shipped file before this: 8 rings cut themselves at the world view
/// (the widest loop 0.463 px², one of them coming out as page grey inside a country) and 2 at
/// ×4.2 (the widest 1.055 px², two holes). After it: none at the world view, one loop of
/// 0.125 px² left at ×4.2 — an eighth of a pixel, under the hairline that is drawn over it.
/// The pass also *saves* work: 14 546 segments at the world view became 11 728.
///
/// The ring's closing vertex is always kept, so a closed ring still closes on the point it
/// closes on. The pass ends with [`seam_to_the_west`] and [`unfold`] on the small rings: a
/// simplifier can fold a coast thinner than its tolerance across itself, and an even-odd fill
/// paints the loop that closes as a hole in the land ([`unfold`] carries the measurement).
fn apart(points: &[Geo], camera: &Camera, tolerance: f32) -> Vec<Point> {
    let keep_apart = tolerance.max(0.05);
    let mut out: Vec<Point> = Vec::with_capacity(points.len().min(2048));
    let mut last: Option<Point> = None;
    for at in points {
        let point = camera.project(*at);
        if let Some(before) = last {
            if (point.x - before.x).abs() < keep_apart && (point.y - before.y).abs() < keep_apart {
                continue;
            }
        }
        out.push(point);
        last = Some(point);
    }
    if let Some(end) = points.last() {
        let close = camera.project(*end);
        if out.last() != Some(&close) {
            out.push(close);
        }
    }
    // The needle pass: see this function's doc for what it costs and what it bought. The
    // endpoints are never removed — `remove` takes the *middle* of the last three — so a closed
    // ring still closes on the point it closes on.
    let needle = keep_apart * 1.5;
    let mut clean: Vec<Point> = Vec::with_capacity(out.len());
    for point in out {
        clean.push(point);
        while clean.len() >= 3 {
            let at = clean.len();
            let (a, c) = (clean[at - 3], clean[at - 1]);
            if (c.x - a.x).abs() < needle && (c.y - a.y).abs() < needle {
                clean.remove(at - 2);
            } else {
                break;
            }
        }
    }
    if spans_at_most(&clean, UNFOLD_BOX) {
        seam_to_the_west(&mut clean);
        unfold(&mut clean, UNFOLD_WINDOW);
    }
    clean
}

/// The largest ring, in pixels on either side, that is repaired for folds.
///
/// The gate is the visibility of the fault, not the cost of the repair: a loop a reader can see
/// inside a ring is a sizeable share of that ring — the one measured was a 78.5 px² hole in a
/// 16 × 13 px island — while a fold in a ring hundreds of pixels across is a sliver of a few
/// pixels, and a sliver under the hairline is what the stroke covers anyway (the widest loop
/// found in a 423 × 101 px ring was 0.46 px²). Repairing only the small rings also keeps the
/// pass off the vertices that matter to the frame: the world view's long rings hold most of the
/// segments, and the walk below is a windowed lookback per vertex.
const UNFOLD_BOX: f32 = 64.0;

/// Is every side of the ring's box at most `limit` pixels?
fn spans_at_most(points: &[Point], limit: f32) -> bool {
    let Some(first) = points.first().copied() else {
        return false;
    };
    let (mut west, mut east, mut north, mut south) = (first.x, first.x, first.y, first.y);
    for point in points {
        west = west.min(point.x);
        east = east.max(point.x);
        north = north.min(point.y);
        south = south.max(point.y);
    }
    east - west <= limit && south - north <= limit
}

/// Move a closed ring's seam to its westmost vertex, and close it again.
///
/// A ring is a loop: which vertex the file calls the first is arbitrary, and the repair below
/// can only look *backwards* along the drawn order. A fold that straddles the seam — measured
/// once: a 27-point ring whose tail crossed its first segment — is therefore invisible to it.
/// Rotating to the westmost vertex (southmost on a tie) is deterministic, costs one pass, and
/// puts the seam where a fold is least likely; `tests/map_borders.rs` counts what is left.
fn seam_to_the_west(points: &mut Vec<Point>) {
    if points.len() < 4 {
        return;
    }
    let closed = points[0] == points[points.len() - 1];
    let ring = if closed { points.len() - 1 } else { points.len() };
    let mut west = 0usize;
    for index in 1..ring {
        let (here, best) = (points[index], points[west]);
        if here.x < best.x || (here.x == best.x && here.y < best.y) {
            west = index;
        }
    }
    if west == 0 && !closed {
        return;
    }
    let mut turned: Vec<Point> = Vec::with_capacity(points.len());
    turned.extend_from_slice(&points[west..ring]);
    turned.extend_from_slice(&points[..west]);
    let first = turned[0];
    turned.push(first);
    *points = turned;
}

/// How many segments back a fold is looked for: a neck of coast is a handful of vertices long,
/// and a window keeps the repair linear in the ring's own length.
const UNFOLD_WINDOW: usize = 8;

/// Remove the loops a thinning pass folded into a ring, so the ring it hands the fill is simple.
///
/// The tolerance rule above drops a point that is within half a hairline of the one kept before
/// it, and a coast thinner than that — the neck of a spit, the two shores of a creek — can have
/// its two legs sampled at different phases: the drawn line then crosses itself, and the fill,
/// which is even-odd, paints the loop that closes as a **hole in the land**. Measured on the
/// shipped file after the needle pass, at the plate the Overview gives a 1 000 × 470 card: one
/// 27-point ring 16 × 13 px across cut itself into a loop of 78.5 px² — a speck of sea in the
/// middle of an island, at the world view.
///
/// A crossing whose two segments are within [`UNFOLD_WINDOW`] of each other is a fold, not a
/// coastline: the vertices of the loop are removed and the ring goes through the crossing point,
/// which is what the neck would be if it could be drawn at all. A loop longer than the window is
/// left alone — it is a shape, not a thinning artefact, and cutting it would change the map.
///
/// The window is a bound on the work, not on the truth: the pass is one walk with at most
/// `window` segment tests per vertex, and the fills it has repaired are what the painted-coast
/// measurement in `tests/map_borders.rs` counts at every zoom.
fn unfold(points: &mut Vec<Point>, window: usize) {
    if points.len() < 4 {
        return;
    }
    let mut at = 1usize;
    while at < points.len() {
        let mut repaired = false;
        let from = at.saturating_sub(window + 1);
        let mut k = from;
        while k + 1 < at {
            if k + 2 > at {
                break;
            }
            if crosses(points[k], points[k + 1], points[at - 1], points[at]) {
                points.drain(k + 1..at);
                at = k + 2;
                repaired = true;
                break;
            }
            k += 1;
        }
        if !repaired {
            at += 1;
        }
        if points.len() < 4 {
            return;
        }
    }
}

/// Do two segments cross, with each strictly on both sides of the other?
///
/// Touching endpoints and collinear overlaps are not crossings: a ring's own first and last
/// vertex are the same point, and the seams of a coast meet at shared vertices all the time.
fn crosses(a: Point, b: Point, c: Point, d: Point) -> bool {
    // A coast runs mostly one way, so the box test rejects almost every pair before any
    // multiplication: this is what keeps the repair linear enough to run every frame.
    if a.x.max(b.x) < c.x.min(d.x)
        || c.x.max(d.x) < a.x.min(b.x)
        || a.y.max(b.y) < c.y.min(d.y)
        || c.y.max(d.y) < a.y.min(b.y)
    {
        return false;
    }
    let side = |p: Point, q: Point, r: Point| -> f32 {
        (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)
    };
    let (d1, d2) = (side(c, d, a), side(c, d, b));
    let (d3, d4) = (side(a, b, c), side(a, b, d));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// The fewest points that are an *outline*: two are a line across whatever the ring
/// encloses, one draws nothing at all.
///
/// This is the number the coast's guarantee is written in. A thinned ring under three points
/// used to be emitted as it came, and the complaint followed: measured on the vendored file
/// in the 1 000 × 470 card, **287 of the 841 rings stroked at the world view were a dot or a
/// line** (the shortest: one point), 247 at ×1.2, 156 at ×1.5, 88 at ×2, 25 at ×3.2, 3 at
/// ×5.5 — so islands and small countries were invisible at the zooms a reader actually uses,
/// and `frame`'s fill could not draw them either (`path.len() < 3`, below).
const OUTLINE_POINTS: usize = 3;

/// The side of the triangle a ring smaller than the hairline is drawn as: half a pixel, the
/// size of the line it would have been stroked with.
const SPECK_PX: f32 = 0.5;

/// A thinned ring, made drawable: `thinned` when it is an outline, else the ring's own
/// extremes, so a budget may change the detail and never the contents.
///
/// The order matters, and it is cheapest first:
///
/// 1. `thinned`, when the simplifier left three points — the common case, no extra work;
/// 2. otherwise the ring's three extreme projected points: leftmost, rightmost, and the one
///    farthest from the line between them, which is the widest shape three points can put
///    where the ring is. One linear pass over the ring, and only for rings the simplifier
///    collapsed — which are the small ones, so the pass is short;
/// 3. otherwise the ring's own first, middle and last points: a ring that *is* a line (a
///    border piece the antimeridian split left) keeps its line rather than being invented
///    into a shape it does not have;
/// 4. otherwise, a ring the file gave fewer than three coordinates — half a pixel of triangle
///    at the pixel it occupies. A drawing that skipped a visible island is a bug; a drawing
///    that skipped a thousandth of a pixel is a pixel.
///
/// The cost is bounded by the second case and by the number of collapsed rings: at the world
/// view 287 rings, all of them islets a few pixels across (a few dozen coordinates each), and
/// the segments they add are `2 × 287 = 574` on top of the 13 972 the tier already stroked —
/// 14 546 of the 24 000 the frame may spend. Measured after the change, min of five per cell
/// over the 91-cell grid (`tests/map_borders.rs`, `the_frame_cost_of_the_whole_grid_is_measured`,
/// debug build): median 0.678 ms, worst 2.331 ms; the world cell is 1.517 ms for 841 rings.
/// The floor is what a frame pays, and it is a flat line: the extra paths add 574 segments to
/// a 24 000 budget and nothing measurable to the clock (the earlier reading of the same grid
/// was a median of 0.964 ms; the two runs differ by the machine the suite is compiling on, not
/// by this function).
fn outline_of(ring: &[Geo], camera: &Camera, thinned: Vec<Point>) -> Vec<Point> {
    if thinned.len() >= OUTLINE_POINTS {
        return thinned;
    }
    let screen: Vec<Point> = ring.iter().map(|at| camera.project(*at)).collect();
    let extremes = extremes(&screen);
    if extremes.len() >= OUTLINE_POINTS {
        return extremes;
    }
    if screen.len() >= OUTLINE_POINTS {
        let last = screen.len() - 1;
        return vec![screen[0], screen[last / 2], screen[last]];
    }
    // Fewer than three coordinates in the file: the ring is the size of the hairline. Half a
    // pixel of triangle is the most honest shape left, and it is what makes the stroke and
    // the fill draw this island instead of nothing.
    let at = screen.first().copied().unwrap_or(Point::new(0.0, 0.0));
    vec![
        at,
        Point::new(at.x + SPECK_PX, at.y),
        Point::new(at.x, at.y + SPECK_PX),
    ]
}

/// A screen path's three widest points, in the ring's own order: leftmost, rightmost, and the
/// point farthest off the line between them.
///
/// Deduplicated (a ring one pixel across has fewer than three distinct corners), so the caller
/// can read the length as "how many points this ring can offer" and not as "three".
fn extremes(screen: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(OUTLINE_POINTS);
    let Some(first) = screen.first().copied() else {
        return out;
    };
    let (mut west, mut east) = (first, first);
    for point in screen {
        if point.x < west.x {
            west = *point;
        }
        if point.x > east.x {
            east = *point;
        }
    }
    for corner in [west, east] {
        if !out.contains(&corner) {
            out.push(corner);
        }
    }
    // The third corner: however far the ring gets from the line it already has. A ring that
    // is a straight line finds the line itself and adds nothing, which is what the caller's
    // next case is for.
    let mut far: Option<(f32, Point)> = None;
    for point in screen {
        let off = off_line(*point, west, east);
        if far.map_or(true, |(worst, _)| off > worst) {
            far = Some((off, *point));
        }
    }
    if let Some((off, point)) = far {
        if off > 0.0 && !out.contains(&point) {
            out.push(point);
        }
    }
    out
}

/// How far `point` sits off the line through `from` and `to`, in pixels.
fn off_line(point: Point, from: Point, to: Point) -> f32 {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        let (px, py) = (point.x - from.x, point.y - from.y);
        return (px * px + py * py).sqrt();
    }
    ((point.x - from.x) * dy - (point.y - from.y) * dx).abs() / length
}

/// The country code a feature names, in the archive's alphabet: `ISO_A3_EH`, then
/// `ISO_A3`, then `ADM0_A3`.
///
/// Three uppercase ASCII letters or nothing: `-99` is the file saying "no code", and a code
/// the archive could not answer for is a door that opens nothing.
fn code_of(properties: &serde_json::Value) -> String {
    for key in ["ISO_A3_EH", "ISO_A3", "ADM0_A3"] {
        if let Some(value) = properties.get(key).and_then(|value| value.as_str()) {
            let value = value.trim();
            if value.len() == 3 && value.chars().all(|ch| ch.is_ascii_uppercase()) {
                return value.to_string();
            }
        }
    }
    String::new()
}

/// The point the file itself puts this country's name at: `LABEL_X`/`LABEL_Y` in degrees, or
/// `None` when the feature carries neither or a value no place on Earth has.
///
/// Natural Earth chooses these by hand so the label sits on land, inside the country, clear of
/// its neighbours — which is exactly the property a mark that stands for a *country* needs and
/// a centroid cannot promise. Checked against the shipped file: 242 features carry a usable
/// pair, and 230 of them are inside their own outline **by this file's own hit test**; the
/// twelve that are not are the Vatican and San Marino (inside Italy's ring), Lesotho (inside
/// South Africa's), New Zealand (the file's point is in Cook Strait) and islands smaller than
/// the hit test's resolution. That does not make a ring answer wrongly: a press on a ring is
/// answered by the distance to it ([`MARK_HIT_PX`]), so the ring opens its own country even
/// where the land under it belongs to a neighbour.
fn label_of(properties: &serde_json::Value) -> Option<Geo> {
    let lon = properties.get("LABEL_X")?.as_f64()? as f32;
    let lat = properties.get("LABEL_Y")?.as_f64()? as f32;
    let on_earth = lon.is_finite()
        && lat.is_finite()
        && (-180.0..=180.0).contains(&lon)
        && (-90.0..=90.0).contains(&lat);
    on_earth.then_some(Geo::new(lon, lat))
}

/// How much of the plate one feature's rings could cover, as a box: the tiebreak when the file
/// gives one code more than one feature, so the mark sits on the mainland and not on a
/// territory. Zero for a feature with no rings.
fn feature_extent(rings: &[Ring]) -> f32 {
    let (mut west, mut south) = (f32::INFINITY, f32::INFINITY);
    let (mut east, mut north) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for ring in rings {
        west = west.min(ring.bounds.west);
        south = south.min(ring.bounds.south);
        east = east.max(ring.bounds.east);
        north = north.max(ring.bounds.north);
    }
    (east - west).max(0.0) * (north - south).max(0.0)
}

fn push_polygon(value: &serde_json::Value, iso: &str, rings: &mut Vec<Ring>) {
    let Some(ring_list) = value.as_array() else { return };
    for ring in ring_list {
        let Some(points) = ring.as_array() else { continue };
        let mut line: Vec<Geo> = Vec::with_capacity(points.len());
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
        split_at_the_antimeridian(&line, iso, rings);
    }
}

/// Break a ring where it steps over the antimeridian, so no piece draws a line across the
/// whole plate.
///
/// Done once, at parse time, because it is a property of the file and not of the camera:
/// the old frame loop re-split every ring on every frame, at the moment it could least
/// afford the test.
fn split_at_the_antimeridian(points: &[Geo], iso: &str, rings: &mut Vec<Ring>) {
    let mut piece: Vec<Geo> = Vec::with_capacity(points.len());
    for at in points {
        if let Some(before) = piece.last() {
            if (at.lon - before.lon).abs() > ANTIMERIDIAN {
                push_ring(std::mem::take(&mut piece), iso, rings);
                piece.reserve(points.len());
            }
        }
        piece.push(*at);
    }
    push_ring(piece, iso, rings);
}

fn push_ring(points: Vec<Geo>, iso: &str, rings: &mut Vec<Ring>) {
    if points.len() < 2 {
        return;
    }
    let Some(bounds) = Bounds::of(&points) else { return };
    rings.push(Ring {
        points,
        bounds,
        iso: iso.into(),
    });
}

// ------------------------------------------------------------------- the fill

/// One edge of a ring in the fill's sweep: running downwards, with the `x` it starts at,
/// how much `x` moves per pixel of `y`, and the crossing at the row being swept.
#[derive(Clone, Copy)]
struct Edge {
    y0: f32,
    y1: f32,
    start: f32,
    slope: f32,
    /// The crossing at the current row: written once per row, kept between them so the
    /// left-to-right order survives from one row to the next.
    x: f32,
}

/// Keep the crossings left to right, by insertion.
///
/// A coast moves by a fraction of a pixel between two rows, so the list the row before left
/// behind is nearly sorted and this pass is linear — where a fresh sort of the same 11 000
/// crossings cost 6 ms of a 8 ms fill.
fn order_by_x(active: &mut [Edge]) {
    for index in 1..active.len() {
        let mut here = index;
        while here > 0 && active[here - 1].x > active[here].x {
            active.swap(here - 1, here);
            here -= 1;
        }
    }
}

/// The land, as one mesh, filled by the **even-odd** rule over every ring it is given.
///
/// Even-odd is what makes a hole a hole and not a second coat of the same grey: the
/// crossings of *all* rings are paired left to right along each horizontal span, so a lake
/// inside a country cancels the country and shows the page, and an island inside that lake
/// comes back as land without anything having been told which ring is which. No ring is
/// marked "hole" and no winding order is assumed.
///
/// The span is one pixel tall and its two edges are exact at both ends, so the fill is
/// exact along every vertical and stepped only between rows. That step is under the
/// coastline's own hairline, which is drawn on the same path.
pub fn fill_mesh(paths: &[Vec<Pos2>], rect: Rect, colour: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    let (top, bottom) = (rect.top(), rect.bottom());
    if rect.width() <= 0.0 || bottom <= top {
        return mesh;
    }
    let mut edges: Vec<Edge> = Vec::new();
    for path in paths {
        if path.len() < 3 {
            continue;
        }
        for pair in path.windows(2) {
            push_edge(&mut edges, pair[0], pair[1], top, bottom);
        }
        if path[0] != path[path.len() - 1] {
            push_edge(&mut edges, path[path.len() - 1], path[0], top, bottom);
        }
    }
    if edges.is_empty() {
        return mesh;
    }
    edges.sort_by(|left, right| left.y0.total_cmp(&right.y0));

    let rows = (((bottom - top) / FILL_ROW_PX).ceil() as usize).max(1);
    let mut active: Vec<Edge> = Vec::new();
    let mut next = 0usize;
    for row in 0..rows {
        let y_hi = top + row as f32 * FILL_ROW_PX;
        let y_lo = (y_hi + FILL_ROW_PX).min(bottom);
        while next < edges.len() && edges[next].y0 <= y_hi {
            let edge = edges[next];
            next += 1;
            if edge.y1 > y_hi {
                active.push(edge);
            }
        }
        active.retain(|edge| edge.y1 > y_hi);
        if active.len() < 2 {
            continue;
        }
        // The crossings are taken from the across-row order the row before left them in, so
        // this ordering pass is linear: see `order_by_x`.
        for edge in &mut active {
            edge.x = edge.start + (y_hi - edge.y0) * edge.slope;
        }
        order_by_x(&mut active);
        let mut index = 0;
        while index + 1 < active.len() {
            let (left, right) = (active[index], active[index + 1]);
            index += 2;
            let (x0, x1) = (
                left.x.clamp(rect.left(), rect.right()),
                right.x.clamp(rect.left(), rect.right()),
            );
            let (x2, x3) = (
                (right.start + (y_lo - right.y0) * right.slope).clamp(rect.left(), rect.right()),
                (left.start + (y_lo - left.y0) * left.slope).clamp(rect.left(), rect.right()),
            );
            if x1 - x0 <= 0.01 || x2 - x3 <= 0.01 {
                continue;
            }
            let base = mesh.vertices.len() as u32;
            mesh.colored_vertex(Pos2::new(x0, y_hi), colour);
            mesh.colored_vertex(Pos2::new(x1, y_hi), colour);
            mesh.colored_vertex(Pos2::new(x2, y_lo), colour);
            mesh.colored_vertex(Pos2::new(x3, y_lo), colour);
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base, base + 2, base + 3);
        }
    }
    mesh
}

/// Add one edge of a ring, clipped to the plate and skipping the horizontals, which
/// contribute no crossing.
fn push_edge(edges: &mut Vec<Edge>, a: Pos2, b: Pos2, top: f32, bottom: f32) {
    if !(a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite()) {
        return;
    }
    if (a.y - b.y).abs() <= f32::EPSILON {
        return;
    }
    let (a, b) = if a.y < b.y { (a, b) } else { (b, a) };
    if b.y <= top || a.y >= bottom {
        return;
    }
    let slope = (b.x - a.x) / (b.y - a.y);
    let y0 = a.y.max(top);
    let y1 = b.y.min(bottom);
    if y1 <= y0 {
        return;
    }
    let start = a.x + (y0 - a.y) * slope;
    edges.push(Edge { y0, y1, start, slope, x: start });
}

// ------------------------------------------------------------------ the motion

/// The plate's motion: [`crate::gesture::Gesture`] is the one accumulator, and this is the
/// adapter that turns one frame of it into a settled [`Camera`].
///
/// There is no arithmetic of this file's own left in here, and that is the point. The map
/// kept its own accumulator — a zoom step clamped per *frame* whatever `dt` was, a `1e-4`
/// pinch threshold that threw a slow pinch away, and a velocity that `dragged` recorded and
/// `step` then integrated as well, so a drag arrived twice and a 120 px drag moved the
/// picture 263 px — and the graph kept a second copy of the same maths, which is how the
/// same defects were found twice and had drifted apart. The rules (every bound per event,
/// the frame's share as a rate per second, momentum integrated by time, a float noise floor
/// for a pinch) live in `gesture` and are tested there. What is left is the conversion
/// between a gesture's screen points and `geo`'s view-relative ones, and it is the identity:
/// both are measured from the plate's middle, and only their differences and the zoom's
/// anchor are ever read.
pub struct Motion {
    /// The camera as of the last [`Motion::step`], which is where the plate is drawn.
    ///
    /// `geo` clamps every move, so this can never be a camera that shows air beside the
    /// world. Nothing eases toward it any more: the accumulator spends a *rate* per second,
    /// which is the ease, and an ease on top of it is the lag that made a drag arrive late
    /// and coast on after the pointer stopped.
    pub target: Camera,
    gesture: Gesture,
}

impl Motion {
    /// The most zoom the queue may hold, in log units. Read from the accumulator rather than
    /// typed a second time: two bounds would be two answers to "how much may pile up".
    pub const PENDING_MAX: f32 = ZOOM_PENDING_MAX;

    pub fn new(target: Camera) -> Motion {
        Motion {
            target,
            gesture: Gesture::default(),
        }
    }

    /// The box changed. The target is re-seated now, so a resize never waits for someone to
    /// touch the zoom.
    pub fn resize(&mut self, size: (f32, f32)) {
        self.target.resize(size);
    }

    /// A zoom the view measured itself, in log units — a keyboard `+`, a wheel it has
    /// already checked against the camera's own ends — aimed at `anchor`.
    pub fn zoom_by(&mut self, log_step: f32, anchor: Point) {
        self.gesture.zoom_log(log_step, pos(anchor));
    }

    /// One frame's pinch reading, `zoom_delta()`, aimed at `anchor`. Whether it is a pinch at
    /// all is the accumulator's judgement (`Gesture::pinch_reading`), not a threshold here.
    pub fn pinch(&mut self, pinch: f32, anchor: Point) {
        self.gesture.zoom(0.0, pinch, pos(anchor));
    }

    /// Spend a wheel reading of `points` and answer the points the camera could not use.
    ///
    /// A wheel over a plate at its floor has nothing to spend, and the answer is the whole
    /// reading: [`MapView::show`] leaves it in `smooth_scroll_delta`, and egui's `ScrollArea`
    /// reads that field *after* it has drawn its content, so the page scrolls with it. Six
    /// −30 pt notches over the plate therefore scroll the page as far as the same six over
    /// text (178 px), instead of being swallowed by a map with nowhere left to zoom. Above
    /// the floor the plate uses the whole reading and the page holds still.
    pub fn wheel(&mut self, points: f32, anchor: Point) -> f32 {
        if !points.is_finite() || points == 0.0 {
            return 0.0;
        }
        // One event's worth, bounded by the accumulator's own per-event number, so that the
        // part handed back to the page is measured on the scale the part kept was. (Not
        // `gesture::WHEEL_EVENT_MAX`: that is a wheel reading's own bound, and one *page* of a
        // wheel legitimately asks for more than one notch — what keeps a gesture from landing
        // in one jump is the frame's rate, not a ceiling on the event.)
        let asked = (points * WHEEL_GAIN).clamp(-PINCH_EVENT_MAX, PINCH_EVENT_MAX);
        let spend = self.room(asked);
        if spend != 0.0 {
            self.gesture.zoom_log(spend, pos(anchor));
        }
        (asked - spend) / WHEEL_GAIN
    }

    /// How much of `asked` (log units) the plate can use, from the zoom it is at, the burst
    /// already queued and the camera's own ends. A plate at either end has no room, which is
    /// what hands the wheel to the page instead of parking it.
    fn room(&self, asked: f32) -> f32 {
        if asked == 0.0 {
            return 0.0;
        }
        let zoom = self.target.zoom();
        let queued = self.gesture.pending_zoom();
        let after = (zoom * (queued + asked).exp()).clamp(self.floor(), self.ceiling());
        ((after / zoom).ln() - queued).clamp(asked.min(0.0), asked.max(0.0))
    }

    /// The camera's own ends, asked of `geo` rather than typed here: the constant behind the
    /// ceiling is private to `geo` on purpose, so that two views cannot disagree about it.
    fn floor(&self) -> f32 {
        self.probe(1e-6)
    }

    fn ceiling(&self) -> f32 {
        self.probe(1e6)
    }

    /// What `geo` clamps a wild zoom to, read off a copy of the camera.
    fn probe(&self, factor: f32) -> f32 {
        let mut far = self.target;
        far.zoom_at(Point::new(0.0, 0.0), factor);
        far.zoom()
    }

    /// One frame of a press: `down` while a button is held on the plate, `at` the pointer.
    ///
    /// Called every frame, held or not, so a release cannot be missed — a movement without
    /// one would coast for ever. While the button is down the picture follows the pointer
    /// exactly, one point per point; the flick is only *recorded*, and only spent after the
    /// release.
    pub fn press(&mut self, down: bool, at: Point) {
        self.gesture.pointer(down, pos(at));
    }

    /// Is the press a pan yet? A press that has not crossed the accumulator's slop is still a
    /// click, and must still open what it was aimed at.
    pub fn panning(&self) -> bool {
        self.gesture.is_panning()
    }

    /// The plate moves by `by`, exactly — the pointer's own points, no easing and nothing
    /// applied twice. `dt` is the frame the movement happened in and is read only for the
    /// flick a release coasts with: `dt == 0` (a keyboard nudge, a test replaying a drag with
    /// no clock) is a step and not a throw.
    pub fn dragged(&mut self, by: (f32, f32), dt: f32) -> Camera {
        self.gesture.panned(Vec2::new(by.0, by.1), dt);
        self.target.pan(by);
        self.target
    }

    /// The pointer lifted: coast at the speed the last samples had, decaying by time.
    pub fn released(&mut self) {
        self.gesture.release();
    }

    /// Drop what is in flight: a queued zoom and a coast. The `0` key uses it so that "the
    /// whole world" means the whole world and not the whole world plus a pinch still on its
    /// way.
    pub fn forget(&mut self) {
        self.gesture.forget();
    }

    pub fn is_moving(&self) -> bool {
        self.gesture.is_moving()
    }

    /// The coast a release is running, in points per second: the momentum test's own reading.
    #[cfg(test)]
    pub fn velocity(&self) -> (f32, f32) {
        let coast = self.gesture.coast();
        (coast.x, coast.y)
    }

    /// One frame: the gesture's pan, then its share of the zoom. `true` when something is
    /// still moving, i.e. when the window must ask for another frame.
    ///
    /// Both are applied to the camera the caller passes, in the order a pointer would do
    /// them: a pinch that pivots around a point is a different picture if the plate slides
    /// first, and the pointer moved the plate first.
    pub fn step(&mut self, current: Camera, dt: f32) -> (Camera, bool) {
        let step = self.gesture.step(dt);
        let mut camera = current;
        if step.pan != Vec2::ZERO {
            camera.pan((step.pan.x, step.pan.y));
        }
        if step.zoom != 1.0 {
            camera.zoom_at(Point::new(step.anchor.x, step.anchor.y), step.zoom);
        }
        self.target = camera;
        (camera, step.running)
    }
}

/// A gesture's screen position as `geo` measures it: the same point in two types, both
/// offsets from the plate's middle, so this is a conversion and not a coordinate change.
fn pos(at: Point) -> Pos2 {
    Pos2::new(at.x, at.y)
}

/// Is `camera` at its target? A fifth of a pixel of pan and a quarter of a percent of zoom,
/// which is the tolerance the frame's own rounding needs: with nothing easing between the
/// two, the answer is true on every frame the accumulator had nothing left to spend, and the
/// window stops asking for repaints on exactly that frame. Only a test asks it — the window
/// reads `Step::running` — so it is compiled only for one.
#[cfg(test)]
fn arrived(camera: &Camera, target: &Camera) -> bool {
    let (here, there) = (camera.offset(), target.offset());
    (here.x - there.x).abs() < 0.2
        && (here.y - there.y).abs() < 0.2
        && (camera.zoom() - target.zoom()).abs() <= target.zoom() * 0.0025
}

// ---------------------------------------------------------------- the graticule

/// One line every `step` degrees of longitude and latitude, in screen space.
///
/// Drawn under the land, so it is the *water's* grid: the fill is a flat silhouette and not
/// a mesh of lines, which is what makes it read as land. Zoom in and the same 30° lines
/// spread out; the ones that leave the plate are not returned at all. (The two axes are
/// drawn over the fill separately, because a reader still has to see where zero is.)
pub fn graticule(camera: &Camera, centre: Pos2, rect: Rect, step: f32) -> Vec<[Pos2; 2]> {
    let mut out = Vec::new();
    if step <= 0.0 {
        return out;
    }
    let mut lon = -180.0_f32;
    while lon <= 180.0 {
        let top = on_plate(camera, centre, Geo::new(lon, 90.0));
        let bottom = on_plate(camera, centre, Geo::new(lon, -90.0));
        if top.x >= rect.left() - 1.0 && top.x <= rect.right() + 1.0 {
            out.push([top, bottom]);
        }
        lon += step;
    }
    let mut lat = -90.0_f32;
    while lat <= 90.0 {
        let left = on_plate(camera, centre, Geo::new(-180.0, lat));
        let right = on_plate(camera, centre, Geo::new(180.0, lat));
        if left.y >= rect.top() - 1.0 && left.y <= rect.bottom() + 1.0 {
            out.push([left, right]);
        }
        lat += step;
    }
    out
}

/// A place, in the plate's own pixels.
fn on_plate(camera: &Camera, centre: Pos2, at: Geo) -> Pos2 {
    let point = camera.project(at);
    Pos2::new(centre.x + point.x, centre.y + point.y)
}

// ------------------------------------------------------------------ the pins

/// One drawn dot: one place, or several places close enough on screen to be one dot.
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    /// The strongest pin behind this dot: the story a plain click opens.
    pub rep: usize,
    /// **Every** pin behind this dot, heaviest first, `rep` among them.
    ///
    /// The dot stands for these places and nothing outside them: measured on the shipped
    /// archive, the busiest dot of the opening view is 3 places and 99 stories, and city and
    /// country sit on the same coordinate (China + Beijing 88 stories, US + Washington 96),
    /// so no zoom ever separates them. A dot that could name only `rep` therefore hid 98 of
    /// 99 stories behind a click that opened one — `tests/map_audit.rs`
    /// (`a_dot_for_several_places_opens_only_one_of_them`) is the audit that says so.
    pub members: Vec<usize>,
    pub at: Pos2,
    pub places: usize,
    pub stories: i64,
    pub radius: f32,
}

/// Group pins by **screen** distance: no two dots are ever closer than `separation`, so a
/// crowded coast can never become a blob.
///
/// Greedy and deterministic: the heaviest pin claims its dot, and every lighter pin within
/// `separation` joins it. Two dots are therefore at least `separation` apart by
/// construction — the radius grows with the stories merged into it, and nothing else.
pub fn cluster(
    pins: &[Pin],
    at: impl Fn(&Pin) -> Pos2,
    rect: Rect,
    separation: f32,
) -> Vec<Cluster> {
    let mut order: Vec<usize> = (0..pins.len()).collect();
    order.sort_by(|left, right| {
        pins[*right].count.cmp(&pins[*left].count).then(left.cmp(right))
    });
    let mut out: Vec<Cluster> = Vec::new();
    for index in order {
        let pin = &pins[index];
        let point = at(pin);
        if !rect.contains(point) {
            continue;
        }
        if let Some(near) = out
            .iter_mut()
            .find(|cluster| cluster.at.distance(point) < separation)
        {
            near.places += 1;
            near.stories += pin.count;
            near.members.push(index);
            near.at = (near.at + point.to_vec2()) / 2.0;
            near.radius = dot_radius(near.stories);
            continue;
        }
        out.push(Cluster {
            rep: index,
            members: vec![index],
            at: point,
            places: 1,
            stories: pin.count,
            radius: dot_radius(pin.count),
        });
    }
    out
}

/// A dot's radius from the stories it stands for: area, not diameter, so a hundred stories
/// is not a hundred times as wide.
pub fn dot_radius(stories: i64) -> f32 {
    (2.6 + (stories.max(0) as f32).sqrt() * 0.9).clamp(2.6, 9.0)
}

/// What a dot says when the pointer is on it. Sentence case, no counts dressed up.
pub fn cluster_label(places: usize, stories: i64, place: &str) -> String {
    let stories = if stories == 1 {
        "1 story".to_string()
    } else {
        format!("{} stories", nf(stories))
    };
    if places > 1 {
        format!("{} places · {stories}", nf(places as i64))
    } else if place.trim().is_empty() {
        stories
    } else {
        format!("{} · {stories}", place.trim())
    }
}

/// Where the top-left corner of a `size` label goes so that the whole label stays inside
/// `rect`, next to `at`.
///
/// It sits to the right and above the dot when there is room; when there is not it flips to
/// the other side, and it is always nudged back inside — a label that runs off the plate is
/// a label that cannot be read.
pub fn label_pos(at: Pos2, size: Vec2, rect: Rect, gap: f32) -> Pos2 {
    let inside = rect.shrink(4.0);
    let mut x = at.x + gap;
    if x + size.x > inside.right() {
        x = at.x - gap - size.x;
    }
    let mut y = at.y - gap - size.y;
    if y < inside.top() {
        y = at.y + gap;
    }
    Pos2::new(
        x.clamp(inside.left(), (inside.right() - size.x).max(inside.left())),
        y.clamp(inside.top(), (inside.bottom() - size.y).max(inside.top())),
    )
}

/// One label on the plate: every line laid out before the box is placed, so the box is the
/// size of what it holds — a card that grew after it was placed would move under the pointer
/// that made it.
///
/// Shared by the dots and the country rings so the two can never drift into two looks: the
/// first line is what the reader is on, the rest are what the press will do, and both are read
/// from the same place, [`theme::INK`] over [`theme::PAGE`] with a hairline all round.
fn label_box(painter: &egui::Painter, rect: Rect, at: Pos2, gap: f32, lines: &[(String, Color32)]) {
    let galleys: Vec<_> = lines
        .iter()
        .map(|(text, colour)| painter.layout_no_wrap(text.clone(), plate_font(), *colour))
        .collect();
    let width = galleys.iter().map(|galley| galley.size().x).fold(0.0_f32, f32::max);
    let height: f32 = galleys.iter().map(|galley| galley.size().y).sum::<f32>()
        + 2.0 * (galleys.len().saturating_sub(1)) as f32;
    let size = Vec2::new(width, height) + Vec2::new(12.0, 6.0);
    let box_ = Rect::from_min_size(label_pos(at, size, rect, gap), size);
    painter.rect_filled(box_, egui::CornerRadius::same(theme::RADIUS), theme::PAGE);
    painter.rect_stroke(
        box_,
        egui::CornerRadius::same(theme::RADIUS),
        theme::line(),
        StrokeKind::Inside,
    );
    let mut corner = box_.min + Vec2::new(6.0, 3.0);
    for galley in galleys {
        let step = galley.size().y + 2.0;
        painter.galley(corner, galley, theme::INK);
        corner.y += step;
    }
}

// ------------------------------------------------------------ the country marks

/// A country a story names, drawn as a ring where the file puts its name.
///
/// The dots are *places*: one dot per group of place labels, 33 of them in the shipped
/// window. The countries the stories name are 56, and a story filed in Kyiv that also names
/// Germany has one coordinate — the city — so Germany is a country the plate can only reach
/// by pressing its land, which is a target a reader has to guess is one. The mark is that
/// country's own door: its press answers `Pick::Country(iso)`, the same answer the land and
/// the chip give, and the same read (`Archive::country_stories`) follows.
///
/// A mark is a ring and a centre, not a filled dot: a filled dot is a place a story came from,
/// and a country is not that — the drawing says which of the two a reader is looking at
/// before they press it.
#[derive(Clone, Debug, PartialEq)]
pub struct CountryMark {
    /// The code the archive files stories under.
    pub iso: String,
    /// The stories the chip counts, and the length of the list the press opens.
    pub count: i64,
    /// The label point, on the plate.
    pub at: Pos2,
}

/// A mark's ring: the radius of the drawn circle, and the radius a press is answered within.
///
/// The hit radius is the one the dots use (6 px) plus the ring's own width, so a mark is as
/// easy to hit as a dot and easier than the 1 px stroke it is drawn with looks.
pub const MARK_RADIUS: f32 = 3.6;
pub const MARK_HIT_PX: f32 = 8.0;

/// Where each country's mark goes, and which countries there is room for.
///
/// The position is the file's own label point ([`World::label_point`]) and nothing else: it is
/// inside the country by construction, so the mark is never in the sea and never on a
/// neighbour. That is also why it is a legitimate *second* position for a story: a story has
/// one coordinate (its city) and is drawn once as a dot; the mark is the country, drawn as a
/// country, and it says so by being a ring rather than a story dot.
///
/// Marks keep [`CLUSTER_PX`] from every dot and from each other, in `countries`' order — which
/// is the count, so the busiest country keeps its mark when two would collide. Two rings that
/// overlap read as one ring and would answer for the wrong country, so the spacing is a
/// correctness rule, not a taste. Deterministic: the same inputs give the same marks in the
/// same order.
///
/// Measured on the shipped archive at 72 h, plate 1 186 × 411 (the Overview at 1440 × 900):
/// **28 rings of 56 countries**, with the other 28 dropped because a heavier country's ring,
/// a story dot or the edge of the plate is nearer than the spacing — every country keeps its
/// chip in the rollup, which is the door that also works when the plate is too crowded to
/// draw it. The cost of drawing and spacing them is in `tests/map_countries.rs`: two circles
/// per ring on a plate that already built its coastline, a paired median delta of about
/// 0.05 ms over 120 frames.
pub fn country_marks(
    countries: &[(String, i64)],
    world: &World,
    camera: &Camera,
    centre: Pos2,
    rect: Rect,
    dots: &[Cluster],
) -> Vec<CountryMark> {
    let inside = rect.shrink(MARK_HIT_PX);
    let mut marks: Vec<CountryMark> = Vec::new();
    for (iso, count) in countries {
        if iso.is_empty() || *count <= 0 {
            continue;
        }
        let Some(at) = world.label_point(iso) else { continue };
        if !(at.lon.is_finite() && at.lat.is_finite()) {
            continue;
        }
        let point = on_plate(camera, centre, at);
        if !inside.contains(point) {
            continue;
        }
        let clear_of_dots = dots
            .iter()
            .all(|dot| point.distance(dot.at) >= dot.radius.max(MARK_HIT_PX) + CLUSTER_PX);
        let clear_of_marks = marks
            .iter()
            .all(|mark| point.distance(mark.at) >= CLUSTER_PX);
        if clear_of_dots && clear_of_marks {
            marks.push(CountryMark { iso: iso.clone(), count: *count, at: point });
        }
    }
    marks
}

// ------------------------------------------------------------------- the view

/// The world's own rectangle inside the card: the largest 2:1 box that fits, centred.
///
/// The plate keeps the world's aspect, so a square degree is a square on screen and the whole
/// world is on the plate at the opening zoom. That is the one thing `geo::Camera` cannot be
/// asked for: its floor is the zoom that *fills* the box it is given, on the tighter axis, so a
/// box of another aspect always crops one side of the world. Handing it a 2:1 box is the fit
/// the user asked for — `min(width / 360, height / 180)` of the card — with the air beside the
/// plate instead of the world outside it.
///
/// Measured before this, the shipped 1 188 × 309 plate (a 1440 × 900 window, the Overview card
/// at the height the page could spare): fitted to the world's *width* at 3.30 px/°, it kept
/// latitudes up to ±46.8° — Hamburg (54.0 N) and Vancouver (49.3 N) were off the map. After:
/// 309 / 180 = 1.72 px/°, the world 618 × 309 in the middle of the card, both poles on the
/// plate, and the card's own width left as page on either side.
fn world_box(card: Rect) -> Rect {
    let width = card.width().max(1.0);
    let height = card.height().max(1.0);
    // the largest 2:1 box that fits: width-limited on a wide card, height-limited on a tall one
    let framed = width.min(height * 2.0).max(1.0);
    Rect::from_center_size(card.center(), Vec2::new(framed, (framed / 2.0).max(1.0)))
}

/// The plate's text. A `Painter` takes a `FontId`, not a `RichText`, so the theme's caption
/// role is read here instead of its size being typed a second time.
fn plate_font() -> egui::FontId {
    egui::FontId::new(theme::size::CAPTION, theme::Weight::Light.family())
}

/// What a click on the plate landed on.
///
/// The plate can answer three things because it *is* three things: a dot standing for one
/// place, a dot standing for several, and land with no dot on it — which is a country, and
/// the reader's complaint was that clicking Germany did nothing at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// A dot that stands for one place: the pin's index in the slice the plate was drawn with.
    Place(usize),
    /// A dot that merges several places: every pin under it, heaviest first. A list of one is
    /// a [`Pick::Place`].
    Places(Vec<usize>),
    /// Land with nothing on it: the country, by ISO-3166-1 alpha-3, the code the archive files
    /// its stories under (`Archive::country_stories`). Empty never reaches here.
    Country(String),
}

/// The map view's state: the outlines, the camera, and whatever is still moving.
pub struct MapView {
    world: World,
    camera: Camera,
    motion: Motion,
    framed: bool,
    /// The dot the reader picked, if any: its label stays on the plate.
    selected: Option<usize>,
    /// The last frame's segment count, for the caption and the test.
    segments: usize,
    /// The rectangle the last frame drew the world in, in the card's own coordinates: the 2:1
    /// plate [`world_box`] seats inside the card. Kept like `segments` because it is a fact
    /// about what was painted, and the measurement of "is the whole world on the plate" needs
    /// the plate the frame actually used rather than a second copy of the rule that made it.
    plate: Option<Rect>,
    hovered: Option<usize>,
    /// What the last press landed on: a dot, a merged dot, or a country. `show` still answers
    /// the representative pin (its callers open one story), and this is the whole answer —
    /// `overview::map_card` reads it to open a list instead of a row.
    picked: Option<Pick>,
    /// Whether the frame just drawn carried a press. A page acts on a pick once, in the frame
    /// the reader pressed; `picked` outlives it so the list a merged dot opens stays on screen
    /// while the reader chooses from it, and a page that came back to the Overview later does
    /// not act on the press it made an hour ago.
    pressed: bool,
    /// The country marks the last frame drew, and the one the pointer or the keyboard is on.
    ///
    /// Kept on the view rather than returned because they are the plate's *contents*, like
    /// `segments`: a test reads what was drawn and presses it, and the label the pointer makes
    /// has to survive the frame that made it.
    marks: Vec<CountryMark>,
    hovered_mark: Option<usize>,
    /// The mark the keyboard walked to. `selected` stays the dot; a plate can be on one or the
    /// other, and never on both.
    selected_mark: Option<usize>,
    /// The land's fill, kept for as long as the plate draws the same geometry.
    ///
    /// `fill_mesh` sorts every edge of every ring and steps it into one-pixel rows. Measured
    /// release at the world view (835 rings, 15 343 vertices, since the fill stopped capping
    /// rings) that is 0.63 ms of a steady Overview frame's 1.7–2.7 ms — a third of a frame a
    /// reader sees spent rebuilding a mesh nothing on the page moved. The mesh is a function
    /// of the camera and the card's rect (`World::frame` is immutable after the parse), and a
    /// steady frame changes neither, so it is built when one of them changes and cloned out
    /// of the cache afterwards: cloning 15 343 vertices is a memcpy, an order under the
    /// build. A stale mesh would draw land in the wrong place, so both halves of the key are
    /// compared exactly, and any change drops it.
    fill: Option<FillCache>,
}

/// One built fill, keyed by everything it depends on.
struct FillCache {
    camera: Camera,
    rect: Rect,
    mesh: Mesh,
}

impl MapView {
    pub fn new(path: &Path) -> MapView {
        let world = World::load(path);
        // a placeholder box until the first frame says how big the card is; `Camera::new`
        // seats the world in it, so nothing is ever drawn at a scale that cannot happen
        let camera = Camera::new((1000.0, 470.0));
        MapView {
            world,
            camera,
            motion: Motion::new(camera),
            framed: false,
            selected: None,
            segments: 0,
            plate: None,
            hovered: None,
            picked: None,
            pressed: false,
            marks: Vec::new(),
            hovered_mark: None,
            selected_mark: None,
            fill: None,
        }
    }

    /// The plate the last frame drew the world in, in the card's coordinates, or `None` before
    /// the first frame. The readout the tests hold the drawing to, like [`MapView::segments`].
    pub fn plate_rect(&self) -> Option<Rect> {
        self.plate
    }

    /// What the outlines cost to read: coastlines, points kept and the milliseconds the
    /// parse took. Said in words on the card, because a plate that took a second to load is
    /// a fact about the window, not a secret.
    pub fn outlines(&self) -> String {
        format!(
            "{} coastlines · {} points · outlines read in {:.0} ms",
            nf(self.world.lines() as i64),
            nf(self.world.points() as i64),
            self.world.parse_ms
        )
    }

    /// Segments drawn in the last frame.
    pub fn segments(&self) -> usize {
        self.segments
    }

    /// The camera the plate was last drawn with. A test that walks a pinch, a wheel or a drag
    /// has to read the picture and not the accumulator: what the reader sees *is* the camera.
    pub fn camera(&self) -> Camera {
        self.camera
    }

    /// The dot that was picked, if any. Esc clears it (`show`), which is the keyboard's way
    /// out of a pin.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// What the last press landed on: one place, several places, or a country.
    ///
    /// [`MapView::show`] answers the representative pin, because the window opens one story
    /// from a dot; this is the rest of the answer, and it is `None` for open water and for a
    /// press that was the end of a drag.
    pub fn picked(&self) -> Option<&Pick> {
        self.picked.as_ref()
    }

    /// Did the frame just drawn carry a press? True for one frame, so a page that opens a
    /// list on a pick does it exactly once.
    pub fn pressed(&self) -> bool {
        self.pressed
    }

    /// The country marks the last frame drew: the rings on the plate, heaviest country first.
    ///
    /// A mark is a door, so a test (and a caller that wants to say how many countries the
    /// plate holds) reads them from here rather than recomputing the spacing and risking a
    /// second answer to the same question.
    pub fn marks(&self) -> &[CountryMark] {
        &self.marks
    }

    /// Which mark the plate speaks about this frame: the pointed-at one, the one the keyboard
    /// walked to, or the one whose list is open — in that order, so the pointer's card never
    /// covers the keyboard's choice.
    fn mark_label(&self) -> Option<usize> {
        self.hovered_mark
            .or(self.selected_mark)
            .or_else(|| match &self.picked {
                Some(Pick::Country(iso)) => {
                    self.marks.iter().position(|mark| mark.iso == *iso)
                }
                _ => None,
            })
            .filter(|index| *index < self.marks.len())
    }

    /// Draw the plate. Returns the pin a click landed on, if any.
    ///
    /// A card can name the place (`Pin::place`) and the story it opens only by its id: the
    /// headline lives in the archive and a `Pin` does not carry it. A caller that has the
    /// headlines calls [`MapView::show_titled`] and the card reads them out.
    pub fn show(&mut self, ui: &mut egui::Ui, pins: &[Pin], height: f32) -> Option<usize> {
        self.show_titled(ui, pins, &[], height)
    }

    /// The same plate, with the headline of each pin's story (aligned with `pins`; a short
    /// slice or an empty string simply means "no headline", and the card names the id).
    pub fn show_titled(
        &mut self,
        ui: &mut egui::Ui,
        pins: &[Pin],
        titles: &[String],
        height: f32,
    ) -> Option<usize> {
        self.plate(ui, pins, titles, &[], height)
    }

    /// The plate with the countries drawn too: one ring per country a story names, at the
    /// position the file gives that country's name ([`CountryMark`]).
    ///
    /// A press on a ring is the answer a press on that country's land gives —
    /// `Pick::Country(iso)` — so the card opens the same list from either, and a country with
    /// no dot on it is a place a reader can see and press rather than land they have to guess
    /// is pressable. `countries` is `Archive::countries`' rows, in its order, so the busiest
    /// country keeps its mark when two would collide.
    pub fn show_countries(
        &mut self,
        ui: &mut egui::Ui,
        pins: &[Pin],
        countries: &[(String, i64)],
        height: f32,
    ) -> Option<usize> {
        self.plate(ui, pins, &[], countries, height)
    }

    fn plate(
        &mut self,
        ui: &mut egui::Ui,
        pins: &[Pin],
        titles: &[String],
        countries: &[(String, i64)],
        height: f32,
    ) -> Option<usize> {
        let (response, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), height), Sense::click_and_drag());
        // The card keeps the height the page gave it; the *plate* is the world's own 2:1 box
        // inside it, painted on the page's colour, so the card has no empty frame around a
        // shrunken map — see [`world_box`].
        let card = response.rect;
        let rect = world_box(card);
        painter.rect_filled(card, egui::CornerRadius::ZERO, theme::PAGE);
        // Everything the plate draws is clipped to the plate: with the world letterboxed inside
        // the card, a coast at the top of the world would otherwise be painted into the air
        // above it once the reader zooms in.
        let painter = painter.with_clip_rect(rect);
        self.plate = Some(rect);
        let centre = rect.center();
        let size = (rect.width(), rect.height());

        if !self.framed {
            self.framed = true;
            self.camera = Camera::new(size);
            self.motion = Motion::new(self.camera);
        } else {
            // Every frame, not just the first: a window resize or a page switch changes the
            // box, and a camera left at the old scale is a map that is letterboxed or
            // cropped until someone touches the zoom. `resize` keeps the centre and clamps
            // the scale into [the fit, the ceiling]; on an unchanged box it is the identity.
            self.camera.resize(size);
            self.motion.resize(size);
        }

        let dt = ui.input(|input| input.stable_dt).clamp(1.0 / 240.0, 0.05);
        let focus = response.has_focus();
        let mut clicked = None;
        self.pressed = false;
        let pointer = response.hover_pos();
        let anchor = pointer
            .map(|at| Point::new(at.x - centre.x, at.y - centre.y))
            .unwrap_or(Point::new(0.0, 0.0));

        // ---- one gesture, read once: the plate owns the pointer while it is over it
        //
        // What this block does *not* do is swallow the wheel. A plate already at its floor
        // has nothing to spend, and a wheel it eats there is a page that cannot be scrolled
        // by two fingers — measured: 180 pt over text scrolled 178 px, the same 180 pt over
        // the plate scrolled 0 and zoomed 0. So the reading is spent first and only the part
        // the camera could use is taken; the remainder is written back into
        // `smooth_scroll_delta`, which egui's `ScrollArea` reads after it has drawn this
        // content, and the page scrolls with it.
        if response.hovered() {
            let (scroll, pinch) = ui.input(|input| (input.smooth_scroll_delta.y, input.zoom_delta()));
            if Gesture::pinch_reading(pinch).is_some() {
                // a ctrl-scroll arrives in both readings and a page cannot be pinched, so
                // pinching is the one gesture that takes the whole reading: handing half of
                // it to the page would scroll the page out from under the fingers
                self.motion.pinch(pinch, anchor);
                ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
            } else if scroll != 0.0 {
                let left = self.motion.wheel(scroll, anchor);
                ui.input_mut(|input| input.smooth_scroll_delta.y = left);
            }
        }

        // ---- the press, every frame: a release must never be missed, and a press that
        // wanders inside the accumulator's slop is still a click
        let at = ui.input(|input| input.pointer.latest_pos()).or(pointer);
        let held = response.is_pointer_button_down_on() || response.dragged();
        if let Some(at) = at.or(response.hover_pos()) {
            self.motion
                .press(held, Point::new(at.x - centre.x, at.y - centre.y));
        }

        // The keyboard, on a plate that has the focus.
        //
        // **Left and right walk the dots**, and the order is the order of the slice the plate
        // was drawn with — `Archive::pins` answers `n DESC, label ASC`, so the walk is the
        // heaviest place first and the same on every frame and every run (a walk over a
        // screen order would reorder itself every time the camera eased). It wraps, because a
        // key that sticks at the last dot is a dead key. **Enter opens** the picked dot's
        // story, which is the keyboard's press. Up and down still nudge the plate, so a
        // keyboard reader who has zoomed in can move the picture without a pointer.
        let mut opened: Option<usize> = None;
        if focus {
            let mut by = (0.0_f32, 0.0_f32);
            let mut log_step = 0.0_f32;
            let mut walk = 0_i32;
            let mut enter = false;
            ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowLeft) {
                    walk -= 1;
                }
                if input.key_pressed(egui::Key::ArrowRight) {
                    walk += 1;
                }
                if input.key_pressed(egui::Key::ArrowUp) {
                    by.1 += rect.height() * 0.08;
                }
                if input.key_pressed(egui::Key::ArrowDown) {
                    by.1 -= rect.height() * 0.08;
                }
                if input.key_pressed(egui::Key::Enter) {
                    enter = true;
                }
                if input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals) {
                    log_step += ZOOM_STEP_MAX;
                }
                if input.key_pressed(egui::Key::Minus) {
                    log_step -= ZOOM_STEP_MAX;
                }
            });
            // The walk covers every door the plate draws: the dots first, in the slice's own
            // order, then the country rings in `countries`' order. A reader without a pointer
            // has to be able to reach a country too, or a story that only a ring names is a
            // story the keyboard cannot open.
            let doors = pins.len() + self.marks.len();
            if walk != 0 && doors > 0 {
                let last = doors as i32;
                let from = match (self.selected, self.selected_mark) {
                    (_, Some(mark)) => (pins.len() + mark) as i32,
                    (Some(pin), None) => pin as i32,
                    (None, None) => -1,
                };
                // `unwrap_or(-1)` above and `+ last` here is a wrap in both directions: the
                // first step picks the first dot, and stepping past either end starts over.
                let next = (from + walk).rem_euclid(last) as usize;
                if next < pins.len() {
                    self.selected = Some(next);
                    self.selected_mark = None;
                    self.picked = Some(Pick::Place(next));
                } else {
                    self.selected = None;
                    let mark = next - pins.len();
                    self.selected_mark = Some(mark);
                    if let Some(mark) = self.marks.get(mark) {
                        self.picked = Some(Pick::Country(mark.iso.clone()));
                    }
                }
            }
            if enter {
                if let Some(index) = self.selected {
                    if pins.get(index).is_some_and(|pin| pin.article > 0) {
                        opened = Some(index);
                    }
                } else if self.selected_mark.is_some() {
                    // A ring is a list, not a story: Enter is the pointer's press, and the page
                    // opens what `picked` says.
                    self.pressed = true;
                }
            }
            if by != (0.0, 0.0) {
                self.motion.dragged(by, 0.0);
            }
            if log_step != 0.0 {
                self.motion.zoom_by(log_step, Point::new(0.0, 0.0));
            }
            if ui.input(|input| input.key_pressed(egui::Key::Num0)) {
                self.motion.target = Camera::new(size);
                self.motion.forget();
            }
        }

        // Esc is the way back out of a pin without leaving the page, and it is read outside
        // the focus gate on purpose: egui surrenders focus on the Escape key *before* any
        // widget runs (`egui-0.37.0/src/memory/mod.rs:597` clears `focused_widget`), so a
        // focused plate would never see the key. A plate showing a picked dot is the plate the
        // reader is working with; clearing the dot takes the focus back, so the arrows still
        // work afterwards.
        if ui.input(|input| input.key_pressed(egui::Key::Escape))
            && (focus || self.selected.is_some() || self.selected_mark.is_some())
        {
            self.selected = None;
            self.selected_mark = None;
            ui.memory_mut(|memory| memory.request_focus(response.id));
        }
        if response.clicked() {
            ui.memory_mut(|memory| memory.request_focus(response.id));
        }

        // ---- one frame of motion: momentum, the gesture's share of the zoom, the ease
        let (next, moving) = self.motion.step(self.camera, dt);
        self.camera = next;
        if moving {
            // the window stops asking after the plate settles; a gesture keeps it awake
            ui.ctx().request_repaint();
        }

        // ---- the plate: the water's grid, the land, the axes, the coasts
        let grid = Stroke::new(theme::HAIRLINE_WIDTH, theme::HAIRLINE_SOFT);
        for [from, to] in graticule(&self.camera, centre, rect, GRATICULE_STEP) {
            painter.line_segment([from, to], grid);
        }

        let built = self.world.frame(&self.camera, centre);
        self.segments = built.segments;
        // The fill is the same mesh for as long as the camera and the card are, and a steady
        // frame moves neither: measured release, rebuilding it every paint was 0.63 ms of a
        // 1.7–2.7 ms frame at the world view. See `FillCache`.
        let mesh = match &self.fill {
            Some(cache) if cache.camera == self.camera && cache.rect == rect => cache.mesh.clone(),
            _ => {
                let mesh = fill_mesh(built.fill(), rect, LAND);
                self.fill = Some(FillCache {
                    camera: self.camera,
                    rect,
                    mesh: mesh.clone(),
                });
                mesh
            }
        };
        if !mesh.is_empty() {
            painter.add(egui::Shape::mesh(mesh));
        }

        // the equator and the prime meridian: the graticule's own axes, one shade darker and
        // drawn over the fill, so the plate says where zero is across land too
        let axis = theme::line();
        painter.line_segment(
            [
                on_plate(&self.camera, centre, Geo::new(-180.0, 0.0)),
                on_plate(&self.camera, centre, Geo::new(180.0, 0.0)),
            ],
            axis,
        );
        painter.line_segment(
            [
                on_plate(&self.camera, centre, Geo::new(0.0, 90.0)),
                on_plate(&self.camera, centre, Geo::new(0.0, -90.0)),
            ],
            axis,
        );

        let coast = Stroke::new(theme::HAIRLINE_WIDTH, theme::DIM);
        for path in &built.paths {
            if path.len() >= 2 {
                painter.add(egui::Shape::line(path.clone(), coast));
            }
        }

        let at = |pin: &Pin| {
            on_plate(&self.camera, centre, Geo::new(pin.lon as f32, pin.lat as f32))
        };
        let dots = cluster(pins, at, rect, CLUSTER_PX);
        self.hovered = pointer.and_then(|point| {
            dots.iter()
                .position(|dot| point.distance(dot.at) <= dot.radius.max(6.0))
        });
        for (index, dot) in dots.iter().enumerate() {
            let hot = self.hovered == Some(index) || self.selected == Some(dot.rep);
            painter.circle_filled(dot.at, dot.radius, theme::INK);
            if dot.places > 1 {
                // a merged dot says how many places it carries, so a cluster never reads as
                // one place
                painter.circle_stroke(
                    dot.at,
                    dot.radius + 2.0,
                    Stroke::new(theme::HAIRLINE_WIDTH, theme::DIM),
                );
            }
            if hot {
                painter.circle_stroke(dot.at, dot.radius + 4.0, theme::focus_stroke());
            }
        }

        // ---- the countries: one ring where the file puts each country's name
        //
        // Drawn after the dots so a place is never hidden behind a country, and hit-tested
        // after them for the same reason. A country with no dot of its own is the whole point:
        // the press it answers is the press its land answers, so a reader who presses the ring
        // and a reader who presses the land open the same list.
        self.marks = country_marks(countries, &self.world, &self.camera, centre, rect, &dots);
        self.hovered_mark = if self.hovered.is_some() {
            None
        } else {
            pointer.and_then(|point| {
                self.marks
                    .iter()
                    .position(|mark| point.distance(mark.at) <= MARK_HIT_PX)
            })
        };
        for (index, mark) in self.marks.iter().enumerate() {
            let picked = matches!(&self.picked, Some(Pick::Country(iso)) if *iso == mark.iso);
            let hot = self.hovered_mark == Some(index) || self.selected_mark == Some(index) || picked;
            // Two hollow rings: a **filled** dot is a place a story came from, and a country is
            // not that. The inner ring marks the label point, the outer one says "this is an
            // area, and it is pressable" — and neither is ever mistaken for a story dot by a
            // reader or by the window's own shape reading (`tests/e2e_map_click.rs` counts
            // filled circles as dots).
            painter.circle_stroke(
                mark.at,
                1.3,
                Stroke::new(theme::HAIRLINE_WIDTH, theme::DIM),
            );
            painter.circle_stroke(
                mark.at,
                MARK_RADIUS,
                Stroke::new(theme::HAIRLINE_WIDTH, theme::INK),
            );
            if hot {
                painter.circle_stroke(mark.at, MARK_RADIUS + 3.0, theme::focus_stroke());
            }
        }
        // A click is a click only while the press stayed inside the accumulator's slop: the
        // end of a drag that happened to finish over a dot must not open that dot's story.
        if response.clicked() && !self.motion.panning() {
            // A press answers with the whole of what it landed on: one place, several places,
            // or — where there is no dot at all — the country whose land it is. Water answers
            // nothing, and a country whose story list is empty is still named, so the page can
            // say "No stories from Aruba in the last 72 h" instead of going blank.
            self.picked = None;
            self.pressed = true;
            // The dot is looked for at the *press*, not at the frame's `hover_pos`: egui
            // reports the pointer's move and its press in one frame, and a press frame whose
            // hover has not been recomputed would fall through to the land under the dot and
            // open the country instead of the story (measured: a press on Madrid's dot opened
            // "0 from DZA"). One position, two hit tests, the dot first.
            let pressed_at = response.interact_pointer_pos();
            let hit = pressed_at.and_then(|pos| {
                dots.iter()
                    .position(|dot| pos.distance(dot.at) <= dot.radius.max(6.0))
            });
            if let Some(index) = hit {
                let dot = &dots[index];
                self.selected = Some(dot.rep);
                self.selected_mark = None;
                // The story the press opens: the dot's own representative while it still has
                // one, else the heaviest place behind it that does (see [`door`]). `None` when
                // the whole dot is out of stories — a press that opens nothing, never a press
                // that opens a story nobody meant.
                clicked = door(dot, pins);
                self.picked = Some(if dot.members.len() > 1 {
                    Pick::Places(dot.members.clone())
                } else {
                    Pick::Place(dot.rep)
                });
            } else if let Some(index) = pressed_at.and_then(|pos| {
                self.marks
                    .iter()
                    .position(|mark| pos.distance(mark.at) <= MARK_HIT_PX)
            }) {
                // A ring is the country, and the press on it is the press on that country's
                // land: one code, one count, one list.
                self.selected = None;
                self.selected_mark = Some(index);
                self.picked = Some(Pick::Country(self.marks[index].iso.clone()));
            } else {
                self.selected = None;
                self.selected_mark = None;
                let country = pressed_at.and_then(|pos| {
                    let screen = Point::new(pos.x - centre.x, pos.y - centre.y);
                    self.world
                        .country_at_plate(&self.camera, screen)
                        .map(str::to_string)
                });
                self.picked = country.map(Pick::Country);
            }
        }

        // ---- the label: the pointed-at door, or the picked one, always inside the plate
        let label = self
            .hovered
            .filter(|index| *index < dots.len())
            .map(|index| (index, true))
            .or_else(|| {
                self.selected.and_then(|rep| {
                    dots.iter()
                        .position(|dot| dot.rep == rep)
                        .map(|index| (index, false))
                })
            });
        if let Some((index, _)) = label {
            let dot = &dots[index];
            let name = pins.get(dot.rep).map(|pin| pin.place.as_str()).unwrap_or("");
            let mut lines: Vec<(String, Color32)> =
                vec![(cluster_label(dot.places, dot.stories, name), theme::INK)];
            for extra in card_lines(dot, pins, titles) {
                lines.push((extra, theme::DIM));
            }
            label_box(&painter, rect, dot.at, dot.radius + 5.0, &lines);
        } else if let Some(index) = self.mark_label() {
            let mark = &self.marks[index];
            // One line in the caption's ink and one in the dim: what the ring counts, and what
            // the press does with it. No number without a noun — "3 stories", not "3".
            let lines = vec![
                (cluster_label(1, mark.count, &mark.iso), theme::INK),
                ("the press opens this country's list".to_string(), theme::DIM),
            ];
            label_box(&painter, rect, mark.at, MARK_RADIUS + 5.0, &lines);
        }

        // ---- the caption, and the focus ring a keyboard reader needs
        let caption = caption(self.camera.zoom(), pins.len(), self.segments());
        painter.text(
            rect.left_bottom() + Vec2::new(9.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            caption,
            plate_font(),
            // The caption is a *caption*, in the caption's own ink. It was `FAINT`, which the
            // design audit measured at **3.88:1** on `PAGE` — under the 4.5:1 floor for text,
            // and the only line in the window that was. `src/theme.rs` says it plainly: FAINT
            // is a stroke, not ink (a boundary needs 3:1). `role::CAPTION.colour` is DIM,
            // 6.85–7.12:1 here, and the size was already the caption's (11 px, on the ladder).
            theme::role::CAPTION.colour,
        );
        if focus {
            painter.rect_stroke(
                rect.shrink(1.0),
                egui::CornerRadius::ZERO,
                theme::focus_stroke(),
                StrokeKind::Inside,
            );
        }
        clicked.or(opened)
    }
}

/// The pin a press on `dot` opens: the heaviest place behind it that still has a story.
///
/// A dot's representative is its heaviest pin, and a heavy place can outlive its story: the
/// archive keeps the place's count for the window while its newest article id has gone (`0`),
/// which is the state `tests/hostile_rows.rs` builds on purpose. Pressing such a dot answered
/// `Some(rep)`, the page looked the id up, found nothing, and the press did nothing at all —
/// the "I click the news on the map and nothing appears" report, measured at ×1 with a live
/// story of 40 sitting on the very same dot. The keyboard's Enter already refused a pin whose
/// `article` is `0`; this is the pointer refusing it the same way, and answering the heaviest
/// *live* member of a merged dot instead of going silent.
///
/// `members` is heaviest first ([`cluster`] pushes in that order), so the first live member is
/// the one a reader would have chosen.
fn door(dot: &Cluster, pins: &[Pin]) -> Option<usize> {
    dot.members
        .iter()
        .copied()
        .find(|index| pins.get(*index).is_some_and(|pin| pin.article > 0))
}

/// The lines a card says under the place: the story it opens, or the sentence for a dot whose
/// story is gone, and — for a merged dot — every place it stands for.
///
/// Three defects live in these few lines, all of them measured by `tests/map_card.rs`:
///
/// * a card that named only the place read the same whether the press opened story 11 or story
///   99, so the reader could not see what they were about to open;
/// * a pin whose story is gone (`article = 0`) painted the card of a live door, and the press
///   opened nothing;
/// * a dot that merges places (China + Beijing, US + Washington sit on one coordinate) is a
///   door onto several lists, and this is where the card says which.
///
/// The headline comes from the caller ([`MapView::show_titled`]); with none, the id is what
/// names the story, and the wording says it is a story rather than leaving a bare number.
fn card_lines(dot: &Cluster, pins: &[Pin], titles: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if dot.members.len() > 1 {
        let places: Vec<&str> = dot
            .members
            .iter()
            .filter_map(|index| pins.get(*index))
            .map(|pin| pin.place.trim())
            .filter(|place| !place.is_empty())
            .collect();
        if !places.is_empty() {
            lines.push(crate::views::clip(&places.join(" · "), 68));
        }
    }
    match pins.get(dot.rep) {
        Some(pin) if pin.article > 0 => {
            let headline = titles
                .get(dot.rep)
                .map(|title| title.trim())
                .filter(|title| !title.is_empty());
            match headline {
                Some(title) => lines.push(crate::views::clip(title, 68)),
                None => lines.push(format!("opens story {}", pin.article)),
            }
        }
        // A dot whose story is gone still opens its place's list, so the sentence says what
        // the press will do rather than only what it will not.
        Some(pin) => lines.push(format!(
            "its story has left the archive — {} opens the place's list",
            if pin.count == 1 { "one story" } else { "the stories" }
        )),
        None => {}
    }
    lines
}

/// The plate's own line under the map. It may only promise what `geo` allows.
///
/// At the fit the plate *is* the box — the clamp's slack is `(360·k − w)/2` on x, exactly 0
/// there — so a horizontal drag moves nothing. Measured by the input audit: six +20 px
/// pointer steps at ×1.0 moved the plate 0.0 px, under a caption that said "drag to pan". A
/// caption that promises a gesture the clamp forbids is a defect of its own, so the caveat
/// appears exactly where the slack does: as soon as the zoom is above the fit at all.
///
/// The phrase `drag to pan, scroll to zoom` is kept whole in both readings because it is the
/// sentinel `tests/frame_cost.rs::Painted::paints_the_plate` uses to tell "the plate was
/// painted" from "it was not"; it is a fact about the plate, not a promise about the fit.
fn caption(zoom: f32, places: usize, segments: usize) -> String {
    let head = format!(
        "zoom ×{zoom:.1} · {} places · {} segments",
        nf(places as i64),
        nf(segments as i64)
    );
    if zoom > 1.0 + 1e-4 {
        format!("{head} · drag to pan, scroll to zoom")
    } else {
        format!("{head} · the whole world is the plate · drag to pan, scroll to zoom once you zoom in")
    }
}

/// The Overview's map card: the plate, the place count and the country rollup.
pub fn card(
    ui: &mut egui::Ui,
    view: &mut MapView,
    pins: &[Pin],
    countries: &[(String, i64)],
    hours: i64,
) -> Option<usize> {
    let note = format!(
        "{} place(s) in the last {} h · {}",
        nf(pins.len() as i64),
        hours,
        view.outlines()
    );
    super::card(ui, "World map", &note, |ui| {
        let clicked = view.show(ui, pins, 470.0);
        if !countries.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (iso, count) in countries.iter().take(14) {
                    ui.label(theme::dim(&format!("{} {}", iso, nf(*count))));
                }
            });
        }
        clicked
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plate the map card allocates: 1 000 × 470 pixels, wider than the world's 2:1, so
    /// at the opening zoom the width is exactly filled and the poles are 15 px outside the
    /// box.
    const CARD: (f32, f32) = (1000.0, 470.0);

    fn plate() -> Rect {
        Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(CARD.0, CARD.1))
    }

    fn centre() -> Pos2 {
        plate().center()
    }

    /// A place, in the plate's own pixels — the same projection the view paints through.
    fn screen(camera: &Camera, at: Geo) -> Pos2 {
        on_plate(camera, centre(), at)
    }

    /// The camera at `factor` × the fit with `at` under the middle of the plate.
    fn centred(at: Geo, factor: f32) -> Camera {
        let mut camera = Camera::new(CARD);
        camera.zoom_at(Point::new(0.0, 0.0), factor);
        let point = camera.project(at);
        camera.pan((-point.x, -point.y));
        camera
    }

    /// The far end of the zoom: how far a reader can go in.
    fn ceiling() -> f32 {
        let mut camera = Camera::new(CARD);
        for _ in 0..80 {
            camera.zoom_at(Point::new(0.0, 0.0), 2.0);
        }
        camera.zoom()
    }

    /// Does the plate cover the box on both axes? The invariant that stops the map from
    /// shrinking into an island of coastline with air around it.
    fn covers(camera: &Camera, size: (f32, f32)) -> bool {
        let (west, east) = (
            camera.project(Geo::new(-180.0, 0.0)).x,
            camera.project(Geo::new(180.0, 0.0)).x,
        );
        let (north, south) = (
            camera.project(Geo::new(0.0, 90.0)).y,
            camera.project(Geo::new(0.0, -90.0)).y,
        );
        west <= -size.0 / 2.0 + 0.01
            && east >= size.0 / 2.0 - 0.01
            && north <= -size.1 / 2.0 + 0.01
            && south >= size.1 / 2.0 - 0.01
    }

    /// Is the whole world inside the plate? The opening fit's promise, and the opposite of
    /// [`covers`]: the plate is the world's own 2:1 box ([`world_box`]), so at the floor the
    /// four edges of the world are the four edges of the plate and nothing is cropped.
    fn contains(camera: &Camera, plate: Rect) -> bool {
        let half = (plate.width() / 2.0, plate.height() / 2.0);
        let (west, east) = (
            camera.project(Geo::new(-180.0, 0.0)).x,
            camera.project(Geo::new(180.0, 0.0)).x,
        );
        let (north, south) = (
            camera.project(Geo::new(0.0, 90.0)).y,
            camera.project(Geo::new(0.0, -90.0)).y,
        );
        west >= -half.0 - 0.01
            && east <= half.0 + 0.01
            && north >= -half.1 - 0.01
            && south <= half.1 + 0.01
    }

    /// The distance from `point` to the nearest segment of `line`, in pixels: how far the
    /// drawn coast can be from the coast the file describes.
    fn gap(point: Pos2, line: &[Pos2]) -> f32 {
        let mut best = f32::INFINITY;
        for pair in line.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let along = b - a;
            let length = along.length_sq();
            let t = if length <= f32::EPSILON {
                0.0
            } else {
                (((point - a).dot(along)) / length).clamp(0.0, 1.0)
            };
            best = best.min(point.distance(a + along * t));
        }
        best
    }

    /// Is `at` inside one of the fill's triangles? The fill's only observable.
    fn covers_point(mesh: &Mesh, at: Pos2) -> bool {
        let vertices = &mesh.vertices;
        for triangle in mesh.indices.chunks(3) {
            if triangle.len() < 3 {
                continue;
            }
            let (a, b, c) = (
                vertices[triangle[0] as usize].pos,
                vertices[triangle[1] as usize].pos,
                vertices[triangle[2] as usize].pos,
            );
            let sign = |p: Pos2, q: Pos2, r: Pos2| -> f32 {
                (p.x - r.x) * (q.y - r.y) - (q.x - r.x) * (p.y - r.y)
            };
            let (d1, d2, d3) = (sign(at, a, b), sign(at, b, c), sign(at, c, a));
            let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            if !(negative && positive) {
                return true;
            }
        }
        false
    }

    /// The vendored outlines, or a failure that says where they were looked for.
    fn world() -> World {
        let path = crate::assets::geojson();
        let world = World::load(&path);
        assert!(
            world.lines() > 500,
            "the vendored outlines are missing at {}: {} coastlines, {} points",
            path.display(),
            world.lines(),
            world.points()
        );
        world
    }

    // ------------------------------------------------------- the frame's cost, measured

    /// The most a debug build may spend building one frame of coastlines, in milliseconds.
    ///
    /// Measured by the test below, debug, on a 1 000 × 470 card, best of twelve, over several
    /// runs: 1.5–1.6 ms at the world view (13 934 segments), 0.7–0.9 at country, 0.9–1.4 at
    /// region, 0.6 at the city. 25 ms is fifteen times the worst of those — the factor a debug
    /// build costs over release (`tests/frame_cost.rs` carries the release number, 0.30 ms for
    /// the same sweep) — and a third of the 58.9 ms the regression this guards measured: a
    /// frame that went back to a Douglas–Peucker over the whole file.
    const FRAME_CEILING: f64 = 25.0;

    /// The same ceiling for the land fill's mesh, which is the other half of the plate.
    ///
    /// Measured, best of twelve, debug, over several runs: 8.6–10.8 ms at the world view (163
    /// rings, 11 996 vertices), 7.7–9.2 at country, 1.1–1.5 at region, 0.3–0.4 at the city —
    /// six times a frame at the same zoom, because the mesh is vertices and the coast is
    /// segments. Filling every stroked ring takes the world view from 11 996 to 15 343
    /// vertices, and the mesh measured 10.5–16.0 ms across runs for both fills — the row sweep
    /// barely notices 678 tiny islets, and that is the price of land that is not drawn as a
    /// hole. The fill is bounded by the coast's own segment budget rather than by a wall clock,
    /// so this only has to catch a fill that went quadratic.
    ///
    /// 80 ms, not 35: measured on this machine **while `llama-server` was answering and other
    /// test binaries were running** the same sweep took 39–62 ms (57.26 ms at its worst), so a
    /// 35 ms ceiling failed a fill that had not changed at all — the ceiling was measuring the
    /// machine's load, not the code. 80 ms is still an order of magnitude under a quadratic
    /// fill (the 58.9 ms *coastline* regression was a different path), which is the only thing
    /// this assert exists to catch.
    const FILL_CEILING: f64 = 80.0;

    /// How many samples a timing takes before it is anyone's fault but the machine's.
    const SAMPLES: usize = 12;

    /// The wall clock the search for a cheap sample may spend on one step.
    ///
    /// This is what makes the measurement robust rather than lucky: five samples of a 1.6 ms
    /// frame are 8 ms of work, so a scheduler that deschedules the process for a tenth of a
    /// second can swallow *every* one of them — measured, once, in a full `--lib` run with four
    /// other agents compiling: 100.4 ms for a frame that is 1.6 ms when the machine is quiet.
    /// Given 400 ms of sampling, some slice of it is clean.
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(400);

    /// The cheapest of several timings of `work`, in milliseconds, and how many samples it
    /// took.
    ///
    /// A frame's cost is not the machine's load. So: take at least [`SAMPLES`] samples, and
    /// keep taking them while the best is still over `comfortable` — the caller passes *half*
    /// its ceiling, a number the assertion cannot fire on — and [`BUDGET`] of wall clock is
    /// left. On a quiet machine that is twelve samples and a few tens of milliseconds; on a
    /// busy one it is however many samples fit in the budget, which is what buys a clean
    /// slice.
    ///
    /// What it cannot hide is a regression: a quadratic frame is slow in *every* sample, so
    /// the best of them is still slow and the assertion still fires.
    fn cheapest(
        comfortable: f64,
        mut work: impl FnMut() -> f64,
    ) -> (f64, usize) {
        let started = Instant::now();
        let mut best = f64::INFINITY;
        let mut taken = 0;
        while taken < SAMPLES || (best > comfortable && started.elapsed() < BUDGET) {
            best = best.min(work());
            taken += 1;
        }
        (best, taken)
    }

    #[test]
    fn the_parse_time_and_the_frame_line_count_are_measured() {
        let world = world();
        println!(
            "geojson: {} coastlines, {} points, parsed and read in {:.1} ms",
            world.lines(),
            world.points(),
            world.parse_ms
        );
        assert!(
            world.parse_ms < 2000.0,
            "parsing the vendored outlines took {:.1} ms, which is not a start-up cost",
            world.parse_ms
        );

        // A plate anchored on Europe, where there is always a coast to draw: the question is
        // what a frame costs, at every zoom, on real geometry.
        let rect = plate();
        let mut fewest = usize::MAX;
        let mut most = 0usize;
        for (label, factor) in [
            ("world", 1.0_f32),
            ("country", 4.0),
            ("region", 40.0),
            ("city", 96.0),
        ] {
            let camera = centred(Geo::new(10.0, 50.0), factor);
            // What the frame costs, as the cheapest of [`SAMPLES`] samples — and of more, if
            // the machine is busy: see `cheapest`.
            let mut segments = 0usize;
            let mut paths = 0usize;
            let mut filled = 0usize;
            let mut tier = 0usize;
            let (spent, samples) = cheapest(FRAME_CEILING / 2.0, || {
                let at = Instant::now();
                let frame = world.frame(&camera, centre());
                segments = frame.segments;
                paths = frame.paths.len();
                filled = frame.filled;
                tier = frame.tier;
                at.elapsed().as_secs_f64() * 1000.0
            });
            // The fill's mesh, timed on its own inside a frame it has to share with the plate:
            // the number is the mesh's, not the mesh plus the coast.
            let mut vertices = 0usize;
            let (fill_spent, fill_samples) = cheapest(FILL_CEILING / 2.0, || {
                let frame = world.frame(&camera, centre());
                vertices = frame.fill().iter().map(Vec::len).sum();
                let at = Instant::now();
                let mesh = fill_mesh(frame.fill(), rect, LAND);
                let spent = at.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(
                    mesh.is_empty(),
                    frame.filled == 0,
                    "the fill mesh and the fill's ring count disagree at {label}"
                );
                spent
            });
            println!(
                "frame at {label:<7} (zoom ×{:5.1}): {segments:>6} segments in {paths:>4} paths, tier {tier}, filled {filled} rings / {vertices} vertices, built in {spent:.2} ms ({samples} samples), fill mesh {fill_spent:.2} ms ({fill_samples} samples)",
                camera.zoom()
            );
            assert!(
                segments <= MAX_SEGMENTS || tier + 1 == TIERS.len(),
                "the budget is a tier trigger, not a filter: {segments} segments at tier {tier}"
            );
            assert!(segments > 0, "no coastline drew at {label}");
            // The ceiling is a *debug* build's, fifteen times what this machine measures when
            // it is quiet and a third of the 58.9 ms the regression it guards measured; the
            // sample it is compared against is the cheapest of many, so a stolen slice of the
            // machine is not a dropped frame. See `FRAME_CEILING` and `cheapest`.
            assert!(
                spent < FRAME_CEILING,
                "one frame of coastlines took {spent:.2} ms at {label} (best of {samples}), over \
                 the {FRAME_CEILING} ms ceiling a debug build is allowed"
            );
            // The fill is the coast's own geometry, ring for ring: its vertex count is the
            // frame's segments plus one per ring, and it covers every ring — a ring stroked
            // and not filled is land drawn as a hole (see `World::frame`).
            assert_eq!(
                vertices,
                segments + filled,
                "the fill is not the coast's own vertices at {label}: {vertices} vertices for \
                 {segments} segments in {filled} rings"
            );
            assert_eq!(
                filled, paths,
                "at {label} {paths} rings are stroked and only {filled} are filled"
            );
            // The fill is bounded by that, not by a wall clock: a debug build is five to ten
            // times the release cost of the same sweep, and this assert only has to catch a
            // fill that went quadratic. See `FILL_CEILING`.
            assert!(
                fill_spent < FILL_CEILING,
                "the land fill took {fill_spent:.2} ms at {label} (best of {fill_samples}), over \
                 the {FILL_CEILING} ms ceiling"
            );
            fewest = fewest.min(segments);
            most = most.max(segments);
        }
        // the world view is the cheap one: the tiers are doing the work, not a bigger cap
        assert!(
            most > fewest,
            "zooming changed nothing: {fewest} segments at every zoom"
        );
        assert!(
            fewest < world.points() / 4,
            "the world view drew {fewest} of {} points: nothing was simplified",
            world.points()
        );
    }

    #[test]
    fn a_zoom_sweep_never_loses_a_coast_and_never_leaves_the_plate() {
        // Forty steps up from the fill to the ceiling and forty back — the sweep a reader
        // makes with two fingers. Nothing may pop: the set of rings drawn is exactly the set
        // `Bounds::shows` allows, at every step, so a budget can never remove a coast.
        let world = world();
        let top = ceiling();
        let step = top.powf(1.0 / 40.0);
        // Pinched on the English Channel: a sweep about the middle of the plate would end
        // over open water in the Gulf of Guinea, where drawing nothing is correct and says
        // nothing about popping.
        let anchor = Camera::new(CARD).project(Geo::new(-1.5, 50.5));
        let mut camera = Camera::new(CARD);
        let mut worst = 0.0_f64;
        let mut peak = 0usize;
        for index in 0..=80 {
            if index > 0 && index <= 40 {
                camera.zoom_at(anchor, step);
            }
            if index > 40 {
                camera.zoom_at(anchor, 1.0 / step);
            }
            // A frame a step, and the cheapest of many counts — the same rule as the test
            // above, from the same helper: `SAMPLES` frames, and more while the machine is
            // stealing samples, all inside `BUDGET`.
            let (spent, samples) = cheapest(FRAME_CEILING / 2.0, || {
                let at = Instant::now();
                let _ = world.frame(&camera, centre());
                at.elapsed().as_secs_f64() * 1000.0
            });
            // and one more frame to assert on: cheap, and the cheapest above may have been
            // measured on a frame nothing kept
            let frame = world.frame(&camera, centre());
            let visible = world
                .rings
                .iter()
                .filter(|ring| ring.bounds.shows(&camera))
                .count();
            if index % 8 == 0 || index == 40 {
                println!(
                    "sweep {index:>2}: zoom ×{:5.1}, {} of {} rings on the plate, {:>5} segments, tier {}, {spent:.2} ms",
                    camera.zoom(),
                    frame.paths.len(),
                    world.lines(),
                    frame.segments,
                    frame.tier
                );
            }
            // (b) nothing pops: the drawn set *is* the visible set
            assert_eq!(
                frame.paths.len(),
                visible,
                "step {index} at zoom ×{:.1}: {} of {visible} visible rings drew",
                camera.zoom(),
                frame.paths.len()
            );
            assert!(
                frame.paths.len() > 0,
                "step {index} drew no coastline at all"
            );
            // (a) the plate still covers the box
            assert!(
                covers(&camera, CARD),
                "step {index} at zoom ×{:.1} left air around the world",
                camera.zoom()
            );
            // (c) the frame cost: one ceiling for the whole file, `FRAME_CEILING`, set from
            // what a debug build measures (1.6 ms at the world view) times fifteen, which is
            // still a third of the 58.9 ms the regression it guards measured. The sample is the
            // cheapest of many, so a scheduler blip — 9.81 ms and 100.4 ms were both measured
            // on this machine while other agents were linking — is not a dropped frame.
            worst = worst.max(spent);
            assert!(
                spent < FRAME_CEILING,
                "step {index} at zoom ×{:.1} took {spent:.2} ms (best of {samples}), over the \
                 {FRAME_CEILING} ms ceiling",
                camera.zoom()
            );
            assert!(
                frame.segments <= MAX_SEGMENTS || frame.tier + 1 == TIERS.len(),
                "step {index}: {} segments at tier {}",
                frame.segments,
                frame.tier
            );
            peak = peak.max(frame.paths.len());
        }
        println!("sweep: worst frame {worst:.2} ms, most rings on the plate {peak}");
        // and the sweep ends where it started: the same camera, the same drawing
        assert!(
            (camera.zoom() - 1.0).abs() < 1e-3,
            "the sweep did not come back to the fill: ×{:.3}",
            camera.zoom()
        );
    }

    #[test]
    fn a_resize_keeps_the_plate_over_the_box_and_the_place_at_the_middle() {
        // A page switch or a window drag changes the card; the camera must re-seat itself
        // instead of waiting for someone to touch the zoom.
        let mut camera = centred(Geo::new(13.4, 52.5), 8.0);
        let before = camera.unproject(Point::new(0.0, 0.0));
        camera.resize((860.0, 560.0));
        assert!(
            covers(&camera, (860.0, 560.0)),
            "after the resize the plate no longer covers the box: zoom ×{:.2}",
            camera.zoom()
        );
        let after = camera.unproject(Point::new(0.0, 0.0));
        assert!(
            (before.lon - after.lon).abs() < 0.01 && (before.lat - after.lat).abs() < 0.01,
            "the middle of the plate moved: {before:?} → {after:?}"
        );
        // and a camera at the fill stays at the fill of the *new* box
        let mut whole = Camera::new(CARD);
        whole.resize((860.0, 560.0));
        assert!(covers(&whole, (860.0, 560.0)));
        assert!(
            whole.zoom() >= 1.0,
            "a resize below the fit would letterbox: ×{:.3}",
            whole.zoom()
        );
        // the plate is measured through the new box too
        let world = world();
        let mut resized = camera;
        resized.resize((860.0, 560.0));
        let frame = world.frame(&resized, Pos2::new(430.0, 280.0));
        assert!(frame.segments > 0, "the resized plate drew nothing");
    }

    #[test]
    fn a_coastline_stays_within_a_pixel_of_the_file_at_every_zoom() {
        // `geo::decimate` drops a vertex while it is within half a pixel of the line that
        // replaces it, so the drawn coast never departs from the file by more than that. The
        // zoom only decides how many vertices are needed to say the same thing.
        let mut text =
            String::from(r#"{"features":[{"geometry":{"type":"Polygon","coordinates":[[ "#);
        let steps = 720;
        for step in 0..steps {
            let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
            // a wobbly island: a circle with a second harmonic, so it has curvature at every
            // scale and a straight chord would show
            let radius = 20.0 + 1.5 * (angle * 7.0).sin();
            let lon = 10.0 + radius * angle.cos();
            let lat = 45.0 + radius * angle.sin() * 0.6;
            if step > 0 {
                text.push(',');
            }
            text.push_str(&format!("[{lon:.4},{lat:.4}]"));
        }
        text.push_str("]]}}]}");
        let island = World::from_geojson(&text);
        assert_eq!(island.lines(), 1, "the synthetic island did not parse");

        for factor in [1.0_f32, 4.0, 16.0, 96.0] {
            // Centred on the island's eastern rim, not its middle: at ×96 the island is
            // 11 000 px across and the plate 470 px tall, so a camera on the middle would sit
            // inside the island with no coast on screen at all.
            let camera = centred(Geo::new(30.0, 45.0), factor);
            let frame = island.frame(&camera, centre());
            assert_eq!(frame.paths.len(), 1, "one ring, one path, at ×{factor}");
            let drawn = &frame.paths[0];
            // every point the file gave, in screen space, against the drawn line
            let mut worst = 0.0_f32;
            for at in &island.rings[0].points {
                worst = worst.max(gap(screen(&camera, *at), drawn));
            }
            println!(
                "island at ×{factor:5.1}: {} of {} vertices drawn, worst departure {:.2} px",
                drawn.len(),
                island.points(),
                worst
            );
            assert!(
                worst <= 0.55,
                "the drawn coast is {worst:.2} px from the file at ×{factor}: it is not crisp"
            );
            assert!(drawn.len() >= 12, "an island is still an island at ×{factor}");
        }
    }

    #[test]
    fn a_coastline_is_split_at_the_antimeridian() {
        // one ring that steps from +179 to -179 draws a line across the world unless it is
        // broken: the plate must carry two pieces, not one long streak
        let text = r#"{"features":[{"geometry":{"type":"Polygon","coordinates":[[
            [179.0,10.0],[179.5,11.0],[-179.5,11.0],[-179.0,10.0],[179.0,10.0]]]}}]}"#;
        let world = World::from_geojson(text);
        assert_eq!(world.lines(), 2, "the ring is two pieces, one per side");
        let camera = Camera::new(CARD);
        let frame = world.frame(&camera, centre());
        assert_eq!(frame.paths.len(), 2, "both pieces are on the plate");
        for line in &frame.paths {
            let west = line.iter().map(|point| point.x).fold(f32::INFINITY, f32::min);
            let east = line.iter().map(|point| point.x).fold(f32::NEG_INFINITY, f32::max);
            assert!(
                east - west < plate().width() * 0.5,
                "a piece spans {} px: the antimeridian was not split",
                east - west
            );
        }
    }

    #[test]
    fn a_lake_inside_a_country_is_a_hole() {
        // A square of land with a square lake in the middle, and nothing telling the fill
        // which ring is which: the even-odd rule is what makes the lake a hole.
        let text = r#"{"features":[{"geometry":{"type":"Polygon","coordinates":[
            [[-10.0,10.0],[10.0,10.0],[10.0,30.0],[-10.0,30.0],[-10.0,10.0]],
            [[-4.0,16.0],[4.0,16.0],[4.0,24.0],[-4.0,24.0],[-4.0,16.0]]]}}]}"#;
        let world = World::from_geojson(text);
        let camera = Camera::new(CARD);
        let frame = world.frame(&camera, centre());
        assert_eq!(frame.paths.len(), 2, "the ring and its lake both parsed");
        assert_eq!(frame.filled, 2, "both rings are in the fill");
        let mesh = fill_mesh(frame.fill(), plate(), LAND);
        // half-pixel offsets, so a sample cannot land exactly on a span's edge
        let land = screen(&camera, Geo::new(-7.0, 20.3));
        let lake = screen(&camera, Geo::new(0.0, 20.3));
        let sea = screen(&camera, Geo::new(0.0, 0.3));
        assert!(
            covers_point(&mesh, land),
            "the land between coast and lake is not filled"
        );
        assert!(
            !covers_point(&mesh, lake),
            "the lake was filled as land: it is not a hole"
        );
        assert!(!covers_point(&mesh, sea), "the sea was filled as land");
    }

    #[test]
    fn only_the_rings_on_the_plate_are_filled_and_drawn() {
        // Two islands half a world apart: looking at one must not draw or fill the other.
        let text = r#"{"features":[
            {"geometry":{"type":"Polygon","coordinates":[[[-170.0,0.0],[-150.0,0.0],[-150.0,10.0],[-170.0,10.0],[-170.0,0.0]]]}},
            {"geometry":{"type":"Polygon","coordinates":[[[150.0,0.0],[170.0,0.0],[170.0,10.0],[150.0,10.0],[150.0,0.0]]]}}]}"#;
        let world = World::from_geojson(text);
        assert_eq!(world.lines(), 2);
        let camera = centred(Geo::new(-160.0, 5.0), 20.0);
        let frame = world.frame(&camera, centre());
        assert_eq!(frame.paths.len(), 1, "a ring off the plate was drawn");
        assert_eq!(frame.filled, 1, "a ring off the plate was filled");
        let mesh = fill_mesh(frame.fill(), plate(), LAND);
        assert!(!mesh.is_empty());
        assert!(
            covers_point(&mesh, screen(&camera, Geo::new(-160.0, 5.3))),
            "the island on the plate is not filled"
        );
    }

    /// Where a place sits inside the *window's* plate: the projection, plus that plate's middle.
    fn window_point(camera: &Camera, at: Geo) -> Pos2 {
        let point = camera.project(at);
        window_plate().center() + Vec2::new(point.x, point.y)
    }

    /// One trackpad pinch, as egui reports it: a ctrl-wheel over the plate. Thirty points are
    /// `e^(30/200)` = ×1.16 of zoom, the resolution a Mac touchpad actually sends.
    fn pinch(events: &mut Vec<egui::Event>, step: f32) {
        events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, step),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Trackpad,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        });
    }

    /// One headless frame of the 900 × 620 window at a stated clock, so the same stream can
    /// arrive at 30, 60 or 120 Hz instead of only at a sixtieth.
    fn frame_at(
        ctx: &egui::Context,
        time: f64,
        dt: f32,
        events: Vec<egui::Event>,
        draw: impl FnMut(&mut egui::Ui),
    ) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(900.0, 620.0),
            )),
            time: Some(time),
            predicted_dt: dt,
            events,
            ..Default::default()
        };
        let output = ctx.run_ui(input, draw);
        output.drop_without_applying_deltas();
    }

    /// **Complaint 1** — "beim Zoomen rein und raus skaliert sie nicht immer voll automatisch
    /// und buggt": a pinch is about the place under the pointer, at every step, and its ramp
    /// is the same at any refresh rate.
    ///
    /// Measured: 24 pinch frames of 30 pt (a burst of e^3.6 = ×36.6) land on ×35.12 with the
    /// place under the pointer moving **0.001 px**; the largest zoom one frame applies is
    /// ×1.063 of the ×1.42 the cap allows; and the same six pinches at 30, 60 and 120 Hz settle
    /// on ×2.1170 at every rate. The camera's own `zoom_at` holds the anchor to 0.000 px at
    /// ×1 … ×96 — every zoom but the floor, where the clamp re-centres the whole world inside
    /// the card on purpose.
    #[test]
    fn a_pinch_keeps_the_place_under_the_pointer_and_the_rate_off_the_ramp() {
        let ctx = context();
        let pins = vec![
            pin("Berlin", 52.5, 13.4, 40, 11),
            pin("Paris", 48.8, 2.3, 30, 12),
        ];
        let plate_rect = window_plate();
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        let mut index = 0usize;
        frame(&ctx, index, Vec::new(), |ui| {
            view.show(ui, &pins, CARD.1);
        });
        let pointer = window_point(&view.camera(), Geo::new(13.4, 52.5));
        let held = view.camera().unproject(Point::new(
            pointer.x - plate_rect.center().x,
            pointer.y - plate_rect.center().y,
        ));
        let mut worst = 0.0_f32;
        let mut biggest = 0.0_f32;
        for _ in 0..24 {
            index += 1;
            let before = view.camera().zoom();
            let mut events = vec![egui::Event::PointerMoved(pointer)];
            pinch(&mut events, 30.0);
            frame(&ctx, index, events, |ui| {
                view.show(ui, &pins, CARD.1);
            });
            biggest = biggest.max(view.camera().zoom() / before);
            // the ease keeps spending the queue on the frames after the event
            for _ in 0..3 {
                index += 1;
                frame(&ctx, index, Vec::new(), |ui| {
                    view.show(ui, &pins, CARD.1);
                });
            }
            worst = worst.max(window_point(&view.camera(), held).distance(pointer));
        }
        println!(
            "pinch about the pointer: worst drift {worst:.3} px, biggest frame step ×{biggest:.3}, \
             landed ×{:.2}",
            view.camera().zoom()
        );
        assert!(
            view.camera().zoom() > 20.0,
            "the burst did not zoom: ×{:.2}",
            view.camera().zoom()
        );
        assert!(
            worst <= 1.0,
            "the place under the pointer moved {worst:.3} px during the pinch: the zoom is not \
             anchored where the fingers are"
        );
        assert!(
            biggest <= ZOOM_STEP_MAX.exp() + 1e-3,
            "one frame applied ×{biggest:.3} of zoom, over the ×{:.3} the cap allows",
            ZOOM_STEP_MAX.exp()
        );

        // the same six-event burst at three refresh rates, each settled, lands in one place
        let mut landed: Vec<f32> = Vec::new();
        for hz in [30.0_f32, 60.0, 120.0] {
            // a clock of its own: egui refuses a time that moves backwards, and each rate here
            // starts a fresh window
            let paced_ctx = context();
            let mut paced = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
            let dt = 1.0 / hz;
            let mut clock = 0.0_f64;
            frame_at(&paced_ctx, clock, dt, Vec::new(), |ui| {
                paced.show(ui, &pins, CARD.1);
            });
            for _ in 0..6 {
                clock += dt as f64;
                let mut events = vec![egui::Event::PointerMoved(pointer)];
                pinch(&mut events, 25.0);
                frame_at(&paced_ctx, clock, dt, events, |ui| {
                    paced.show(ui, &pins, CARD.1);
                });
            }
            for _ in 0..(hz as usize).max(60) {
                clock += dt as f64;
                frame_at(&paced_ctx, clock, dt, Vec::new(), |ui| {
                    paced.show(ui, &pins, CARD.1);
                });
            }
            landed.push(paced.camera().zoom());
        }
        let low = landed.iter().copied().fold(f32::INFINITY, f32::min);
        let high = landed.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        println!("the same burst at 30/60/120 Hz lands ×{low:.4} … ×{high:.4}");
        assert!(
            high - low <= high * 0.01,
            "the same six pinches landed ×{low:.4} … ×{high:.4} across 30–120 Hz: the ramp still \
             counts frames instead of seconds"
        );
    }

    /// **Complaint 2** — "die Ländergrenzen verschwimmen dann oder sind nicht immer komplett
    /// sichtbar": a ring the coast strokes is filled, at every zoom.
    ///
    /// Measured before the fix: the fill stopped at a 12 000-vertex budget, so at ×1 it covered
    /// **163 of the 835 rings the coast stroked**, 38 of them at least 4 × 4 px on the plate and
    /// the widest 20.6 px; at ×3 it was 88 of 318, 22 of them ≥ 4 px and the widest 31.5 px.
    /// Land drawn as a hairline around page grey is a hole, and that is what a reader sees as
    /// borders that are not completely there.
    #[test]
    fn every_ring_the_coast_strokes_is_filled_at_every_zoom() {
        let world = world();
        let plate_rect = plate();
        let mut holes = 0usize;
        let mut checked = 0usize;
        for factor in [1.0_f32, 1.6, 3.0, 6.0, 12.0, 40.0, 96.0] {
            for at in [
                Geo::new(10.0, 50.0),
                Geo::new(-3.7, 40.4),
                Geo::new(30.5, 50.4),
                Geo::new(-160.0, 20.0),
            ] {
                let camera = centred(at, factor);
                let frame = world.frame(&camera, centre());
                assert_eq!(
                    frame.filled,
                    frame.paths.len(),
                    "at ×{factor} over {at:?}: {} of {} stroked rings are not filled — a coast \
                     with page grey inside it",
                    frame.paths.len() - frame.filled,
                    frame.paths.len()
                );
                holes += frame.paths.len() - frame.filled;
                // and the land a reader sees is inside the fill mesh, not only counted in it
                let mesh = fill_mesh(frame.fill(), plate_rect, LAND);
                for iso in ["DEU", "FRA", "ITA", "ESP", "GBR", "JPN", "NZL"] {
                    let Some(land) = world.label_point(iso) else {
                        continue;
                    };
                    let point = screen(&camera, land);
                    if !plate_rect.contains(point) {
                        continue;
                    }
                    // Only a point whose whole 3 × 3 px neighbourhood is that country's land:
                    // the fill is swept in one-pixel rows, so a speck thinner than a row is a
                    // shape no scanline may fill, and this test is about land a reader can see.
                    let land_at = |at: Pos2| {
                        world.country_at(camera.unproject(Point::new(
                            at.x - centre().x,
                            at.y - centre().y,
                        )))
                    };
                    let solid = [-1.0_f32, 0.0, 1.0].iter().all(|down| {
                        [-1.0_f32, 0.0, 1.0]
                            .iter()
                            .all(|across| land_at(point + Vec2::new(*across, *down)) == Some(iso))
                    });
                    if !solid {
                        continue;
                    }
                    checked += 1;
                    let inside = [-1.0_f32, 0.0, 1.0].iter().any(|down| {
                        [-1.0_f32, 0.0, 1.0].iter().any(|across| {
                            covers_point(&mesh, point + Vec2::new(*across, *down))
                        })
                    });
                    assert!(
                        inside,
                        "at ×{factor} over {at:?}: the middle of {iso} ({point:?}) is not inside \
                         the fill — the country is an outline around a hole"
                    );
                }
            }
        }
        println!(
            "every stroked ring is filled at 28 zooms: {holes} holes, {checked} country label \
             points checked inside the fill"
        );
        assert!(checked >= 8, "only {checked} label points were solid enough to check");
    }

    /// **Complaint 3** — "wenn ich auf die Nachrichten klicke in der Map, müssen die sofort
    /// angezeigt werden": a press on a dot answers a pin that has a story.
    ///
    /// Measured before the fix: a dot's representative is its heaviest pin, and a heavy place
    /// can outlive its story. The dot over Berlin and a dead pin of 90 stories answered
    /// `Some(2)` — the dead pin, "gone", article 0 — so the page looked up article 0 and opened
    /// *nothing*: the click did nothing at all, with a live story of 40 sitting on the same dot.
    /// Now the same press answers `Some(0)` (Berlin, story 11), and a dot whose only pin is dead
    /// answers `None` instead of a story nobody meant.
    #[test]
    fn a_press_on_a_dot_answers_a_pin_that_has_a_story() {
        let ctx = context();
        let plate_rect = window_plate();
        // the same coordinate twice: the dead pin is the heavier, so it owns the dot
        let pins = vec![
            pin("Berlin", 52.5, 13.4, 40, 11),
            pin("Paris", 48.8, 2.3, 30, 12),
            pin("gone", 52.5, 13.4, 90, 0),
            pin("nowhere", -40.0, -60.0, 7, 0),
        ];
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        let mut index = 0usize;
        frame(&ctx, index, Vec::new(), |ui| {
            view.show(ui, &pins, CARD.1);
        });
        for factor in [1.0_f32, 3.0, 12.0, 40.0] {
            // the camera a reader would be looking at, reached by the view's own pinch — and
            // the fingers stay on Berlin's dot, so the dots the test presses are the dots a
            // reader would be looking at
            while view.camera().zoom() < factor * 0.99 {
                index += 1;
                let on_berlin = window_point(&view.camera(), Geo::new(13.4, 52.5));
                let mut events = vec![egui::Event::PointerMoved(on_berlin)];
                pinch(&mut events, 40.0);
                frame(&ctx, index, events, |ui| {
                    view.show(ui, &pins, CARD.1);
                });
            }
            for _ in 0..60 {
                index += 1;
                frame(&ctx, index, Vec::new(), |ui| {
                    view.show(ui, &pins, CARD.1);
                });
            }
            let camera = view.camera();
            let dots = cluster(
                &pins,
                |pin| window_point(&camera, Geo::new(pin.lon as f32, pin.lat as f32)),
                plate_rect,
                CLUSTER_PX,
            );
            assert!(!dots.is_empty(), "at ×{factor}: no dot on the plate to press");
            let mut said = Vec::new();
            for dot in &dots {
                let answer = click(&mut view, &pins, dot.at, &ctx, &mut index);
                match answer {
                    Some(found) => {
                        assert!(
                            pins[found].article > 0,
                            "at ×{factor}: the press on the dot at {:?} ({} places, rep {} \
                             \"{}\", article {}) answered pin {found} \"{}\" with article {} — the \
                             page opens nothing at all",
                            dot.at,
                            dot.places,
                            dot.rep,
                            pins[dot.rep].place,
                            pins[dot.rep].article,
                            pins[found].place,
                            pins[found].article
                        );
                        assert!(
                            dot.members.contains(&found),
                            "at ×{factor}: the press answered pin {found}, which is not behind \
                             that dot"
                        );
                    }
                    None => assert!(
                        dot.members.iter().all(|index| pins[*index].article == 0),
                        "at ×{factor}: the dot at {:?} answered nothing while pin {} has a story",
                        dot.at,
                        dot.members
                            .iter()
                            .copied()
                            .find(|index| pins[*index].article > 0)
                            .unwrap_or(usize::MAX)
                    ),
                }
                said.push(format!(
                    "rep {} (article {}) -> {answer:?}",
                    dot.rep, pins[dot.rep].article
                ));
            }
            println!("×{factor:.2}: {} dots, answered {said:?}", dots.len());
        }
    }
    /// **Complaint 4** — the zoom limits: a storm of wheel notches stays inside [`Camera`]'s
    /// clamp, ends exactly on it, and hands the rest of the wheel back to the page.
    ///
    /// Measured: 300 notches of +60 pt in one view reach ×96.00 and stay there, the zoom never
    /// leaves 1 … 96, no frame applies more than ×1.42, and the leftover 60.0 pt goes to the
    /// page. The floor case is `at_the_floor_the_wheel_over_the_plate_is_the_pages`.
    #[test]
    fn the_zoom_limits_hold_under_a_storm_at_both_ends() {
        let ctx = context();
        let pins = vec![pin("Berlin", 52.5, 13.4, 40, 11)];
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        let mut index = 0usize;
        frame(&ctx, index, Vec::new(), |ui| {
            view.show(ui, &pins, CARD.1);
        });
        let top = ceiling();
        let mut biggest = 0.0_f32;
        for sign in [1.0_f32, -1.0] {
            for _ in 0..300 {
                index += 1;
                let before = view.camera().zoom();
                let mut events = vec![egui::Event::PointerMoved(window_plate().center())];
                notch(&mut events, 60.0 * sign);
                frame(&ctx, index, events, |ui| {
                    view.show(ui, &pins, CARD.1);
                });
                biggest = biggest.max((view.camera().zoom() / before).max(before / view.camera().zoom()));
                let now = view.camera().zoom();
                assert!(
                    (1.0..=top + 1e-3).contains(&now),
                    "a notch took the zoom to ×{now:.3}, outside 1 … ×{top:.2}"
                );
            }
            for _ in 0..60 {
                index += 1;
                frame(&ctx, index, Vec::new(), |ui| {
                    view.show(ui, &pins, CARD.1);
                });
            }
            let landed = view.camera().zoom();
            let end = if sign > 0.0 { top } else { 1.0 };
            println!("storm {}: landed ×{landed:.4} (the end is ×{end:.2})", if sign > 0.0 { "in" } else { "out" });
            assert!(
                (landed - end).abs() <= end * 0.001,
                "the storm ended on ×{landed:.4}, not on the ×{end:.2} the clamp allows"
            );
            // The whole world is on the plate at the floor — the fit contains it, and the
            // plate is the world's own 2:1 box — and at the ceiling the world still covers
            // the plate, so no gesture can show air beside it.
            let plate = view.plate_rect().expect("the storm drew no plate");
            if sign < 0.0 {
                assert!(
                    contains(&view.camera(), plate),
                    "at ×{landed:.4} the world is not all on the plate {plate:?}: {:?} … {:?}",
                    view.camera().project(Geo::new(-180.0, 0.0)),
                    view.camera().project(Geo::new(180.0, 0.0))
                );
            }
            if sign > 0.0 {
                assert!(
                    covers(&view.camera(), (plate.width(), plate.height())),
                    "at ×{landed:.4} the world does not cover the {}×{} plate",
                    plate.width(),
                    plate.height()
                );
            }
        }
        println!("the storm's biggest single-frame zoom step: ×{biggest:.3}");
        assert!(
            biggest <= ZOOM_STEP_MAX.exp() + 1e-3,
            "one frame applied ×{biggest:.3} of zoom, over the ×{:.3} the cap allows",
            ZOOM_STEP_MAX.exp()
        );
    }

    #[test]
    fn dots_never_form_a_blob() {
        // ten places within a few pixels of each other: one dot, not ten
        let mut pins = Vec::new();
        for step in 0..10 {
            pins.push(Pin {
                place: "DEU".to_string(),
                lat: 51.5 + step as f64 * 0.02,
                lon: 13.4 + step as f64 * 0.02,
                count: 3,
                article: step as i64,
            });
        }
        pins.push(Pin { place: "FRA".into(), lat: 48.8, lon: 2.3, count: 40, article: 99 });
        let rect = plate();
        // Berlin on the middle of the plate at ×8: the ten pins are then under a pixel apart
        // and Paris ~250 px away, which is the case this test is about
        let camera = centred(Geo::new(13.4, 51.5), 8.0);
        let at = |pin: &Pin| screen(&camera, Geo::new(pin.lon as f32, pin.lat as f32));
        let dots = cluster(&pins, at, rect, CLUSTER_PX);
        assert_eq!(dots.len(), 2, "one dot per screen neighbourhood: {dots:?}");
        let merged = dots
            .iter()
            .find(|dot| dot.places > 1)
            .expect("the ten Berlin pins merged");
        assert_eq!(merged.places, 10, "ten places merged into one dot");
        assert_eq!(merged.stories, 30, "and it carries all their stories");
        assert_eq!(merged.rep, 0, "the strongest pin is its representative");
        for (index, left) in dots.iter().enumerate() {
            for right in dots.iter().skip(index + 1) {
                assert!(
                    left.at.distance(right.at) >= CLUSTER_PX,
                    "two dots {} px apart",
                    left.at.distance(right.at)
                );
            }
        }
        // and the words on the dot say which case it is
        assert_eq!(cluster_label(10, 30, ""), "10 places · 30 stories");
        assert_eq!(cluster_label(1, 7, "DEU"), "DEU · 7 stories");
        assert_eq!(cluster_label(1, 1, "DEU"), "DEU · 1 story");
        assert_eq!(cluster_label(1, 0, ""), "0 stories");
    }

    #[test]
    fn a_label_stays_inside_the_plate() {
        let rect = plate();
        let size = Vec2::new(90.0, 20.0);
        for at in [
            Pos2::new(5.0, 5.0),
            Pos2::new(995.0, 5.0),
            Pos2::new(995.0, 465.0),
            Pos2::new(5.0, 465.0),
            rect.center(),
        ] {
            let corner = label_pos(at, size, rect, 8.0);
            let box_ = Rect::from_min_size(corner, size);
            assert!(
                rect.contains_rect(box_),
                "a label at {at:?} went outside the plate: {box_:?}"
            );
            let dot = Rect::from_center_size(at, Vec2::splat(2.0));
            assert!(
                !box_.intersects(dot),
                "and it never covers its own dot: {box_:?}"
            );
        }
    }

    #[test]
    fn momentum_decays_by_time_not_by_frames() {
        // the same flick, watched at 60 Hz and at 144 Hz: the same speed, and the same
        // distance after the same tenth of a second
        let dt_slow = 1.0 / 60.0_f32;
        let dt_fast = 1.0 / 144.0_f32;
        let start = centred(Geo::new(0.0, 0.0), 4.0);
        let mut slow = Motion::new(start);
        let mut fast = Motion::new(start);
        slow.dragged((120.0, 0.0), dt_slow);
        fast.dragged((120.0 * dt_fast / dt_slow, 0.0), dt_fast);
        slow.released();
        fast.released();
        assert!(
            (slow.velocity().0 - fast.velocity().0).abs() < 2.0,
            "the same flick is the same speed: {} then {}",
            slow.velocity().0,
            fast.velocity().0
        );
        // The plate is already where the drag put it when the finger lifts — a drag is 1:1
        // and nothing eases behind it — so the coast is measured from the drag's own end.
        // (It used to be measured from the plate's *opening* camera, because an ease was
        // still catching up; that lag is the defect this replaced.)
        let mut slow_cam = slow.target;
        let mut fast_cam = fast.target;
        for _ in 0..6 {
            slow_cam = slow.step(slow_cam, dt_slow).0;
        }
        for _ in 0..14 {
            fast_cam = fast.step(fast_cam, dt_fast).0;
        }
        // The drag itself moved the target 120 px and 50 px, so the *coast* is the distance
        // past where the finger let go: that is the part that must not depend on how many
        // frames the tenth of a second was chopped into.
        let slow_coast = slow_cam.offset().x - 120.0;
        let fast_coast = fast_cam.offset().x - 50.0;
        println!(
            "coast after a 120 px flick: 6 frames at 60 Hz {slow_coast:.1} px, 14 frames at 144 Hz {fast_coast:.1} px"
        );
        assert!(slow_coast > 100.0, "the flick coasted nowhere: {slow_coast}");
        assert!(
            (slow_coast - fast_coast).abs() < slow_coast * 0.05,
            "six frames of 60 Hz and fourteen of 144 Hz are the same tenth of a second: {slow_coast} then {fast_coast}"
        );
        // and the glide always ends
        let mut frames = 0;
        while slow.is_moving() && frames < 600 {
            slow_cam = slow.step(slow_cam, dt_slow).0;
            frames += 1;
        }
        assert!(!slow.is_moving(), "a coast always ends");
        assert!(
            frames < 60,
            "it ends in {frames} frames, not the rest of the session"
        );
    }

    #[test]
    fn a_gesture_goes_through_the_capped_zoom_and_the_ease() {
        let start = Camera::new(CARD);
        let anchor = Point::new(0.0, 0.0);
        let mut motion = Motion::new(start);
        let mut camera = start;
        // twenty wheel events between two frames
        for _ in 0..20 {
            motion.zoom_by(0.11, anchor);
        }
        let (first, moving) = motion.step(camera, 1.0 / 60.0);
        assert!(moving, "a pending gesture keeps the loop awake");
        let applied = (first.zoom() / start.zoom()).ln();
        assert!(
            applied <= ZOOM_STEP_MAX + 1e-3,
            "one frame applies at most one capped step, applied {applied}"
        );
        camera = first;
        let mut frames = 1;
        while frames < 240 && (motion.is_moving() || !arrived(&camera, &motion.target)) {
            camera = motion.step(camera, 1.0 / 60.0).0;
            frames += 1;
        }
        assert!(frames > 2, "a twenty-event pinch is not one frame of zoom");
        let landed = (camera.zoom() / start.zoom()).ln();
        assert!(
            landed > 1.5,
            "the whole gesture lands: ×{:.2}",
            camera.zoom()
        );
        assert!(
            landed <= Motion::PENDING_MAX + 1e-2,
            "twenty events of 0.11 queued 2.2, more than one gesture: {landed}"
        );
        assert!(
            arrived(&camera, &motion.target),
            "and it comes to rest on its target"
        );
        // zoomed about the anchor, so the place under it did not move
        let before = start.unproject(anchor);
        let after = camera.unproject(anchor);
        assert!(
            (before.lon - after.lon).abs() < 0.5 && (before.lat - after.lat).abs() < 0.5,
            "the anchor moved: {before:?} → {after:?}"
        );
    }

    #[test]
    fn the_world_never_shrinks_inside_the_card() {
        // The bug the reader reported: zooming out past the fit left a 144 px world floating
        // in a 1 000 px plate. The floor is now the zoom that fills the box, and pressing
        // against it does nothing at all.
        let mut camera = Camera::new(CARD);
        for _ in 0..80 {
            camera.zoom_at(Point::new(0.0, 0.0), 0.5);
        }
        assert_eq!(
            camera.zoom(),
            1.0,
            "the world can be scrolled smaller than the card"
        );
        assert!(covers(&camera, CARD), "the plate shows air around the world");
        let before = camera.offset();
        camera.zoom_at(Point::new(0.0, 0.0), 0.5);
        assert_eq!(
            camera.offset(),
            before,
            "pressing against the floor moved the plate"
        );
        assert_eq!(camera.zoom(), 1.0);
        // the deep end stops where the outlines stop answering
        let mut deep = Camera::new(CARD);
        for _ in 0..80 {
            deep.zoom_at(Point::new(0.0, 0.0), 2.0);
        }
        assert_eq!(deep.zoom(), ceiling(), "the zoom stops at the ceiling");
        assert!(covers(&deep, CARD));
    }

    #[test]
    fn the_graticule_is_one_line_every_thirty_degrees() {
        let camera = Camera::new(CARD);
        let lines = graticule(&camera, centre(), plate(), GRATICULE_STEP);
        // 13 meridians (-180..180 by 30) and, because the card is wider than the world's
        // 2:1, the 5 parallels that clear its top and bottom edges (-60..60)
        assert_eq!(lines.len(), 13 + 5, "got {} lines", lines.len());
        let vertical = lines
            .iter()
            .filter(|[from, to]| (from.x - to.x).abs() < 0.01)
            .count();
        assert_eq!(vertical, 13);
        assert_eq!(lines.len() - vertical, 5, "one parallel per 30° of latitude");
        // zoomed in, the lines still on the plate are the ones returned
        let close = centred(Geo::new(0.0, 0.0), 32.0);
        assert!(graticule(&close, centre(), plate(), GRATICULE_STEP).len() < lines.len());
        assert!(
            graticule(&camera, centre(), plate(), 0.0).is_empty(),
            "no step, no graticule"
        );
    }

    #[test]
    fn the_readout_counts_the_coastlines_it_kept() {
        let world = world();
        assert!(
            world.points() > 20_000,
            "the outlines are the full 1:50m geometry"
        );
        let mut view = MapView::new(Path::new(""));
        view.world = world;
        let text = view.outlines();
        assert!(text.contains("coastlines"), "{text}");
        assert!(text.contains("points"), "{text}");
        assert!(text.contains(" ms"), "{text}");
        assert_eq!(view.segments(), 0, "nothing has been drawn yet");
    }


    // ------------------------------------------------- the input, through a real frame
    //
    // Everything below drives `show` through `Context::run_ui` — the same door the window
    // uses — at the size the audit's headless window had (900 × 620). An input defect is a
    // fact about an event stream, so an event stream is what measures it: a test that called
    // `Gesture` directly would prove the accumulator and not the plate.

    /// A context styled as the window styles it.
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx
    }

    /// The plate as the window lays it out: the full width of a 900 px window, `CARD.1` tall.
    fn window_plate() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, CARD.1))
    }

    /// One headless frame of a 900 × 620 window. `index` is the frame's clock, so egui's
    /// `stable_dt` is a real sixtieth of a second instead of zero.
    fn frame(ctx: &egui::Context, index: usize, events: Vec<egui::Event>, draw: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 620.0))),
            time: Some(index as f64 / 60.0),
            events,
            ..Default::default()
        };
        let output = ctx.run_ui(input, draw);
        // no backend here: `epaint` debug-asserts that a texture delta was handled, and this
        // is its documented way of saying "there is none"
        output.drop_without_applying_deltas();
    }

    /// One frame of a *page*: a scroll area with the plate at its top and 1 800 px of text
    /// under it. Answers the page's scroll offset after the frame.
    fn page_frame(ctx: &egui::Context, index: usize, events: Vec<egui::Event>, view: &mut MapView) -> f32 {
        let mut offset = 0.0_f32;
        frame(ctx, index, events, |ui| {
            let page = egui::ScrollArea::vertical().show(ui, |ui| {
                view.show(ui, &[], CARD.1);
                ui.add_space(1800.0);
            });
            offset = page.state.clamped_offset().y;
        });
        offset
    }

    /// The press of a primary button at `at`, after the move that puts the pointer there.
    fn press(at: Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }

    fn release(at: Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }]
    }

    /// A full press and release on the window's plate, answering what the plate answered, and
    /// keeping the frame clock moving so `stable_dt` stays a real frame.
    fn click(
        view: &mut MapView,
        pins: &[Pin],
        at: Pos2,
        ctx: &egui::Context,
        index: &mut usize,
    ) -> Option<usize> {
        *index += 1;
        frame(ctx, *index, press(at), |ui| {
            view.show(ui, pins, CARD.1);
        });
        *index += 1;
        let mut answer = None;
        frame(ctx, *index, release(at), |ui| {
            answer = view.show(ui, pins, CARD.1);
        });
        answer
    }

    /// One notch of the audit's wheel: 30 pt, in the direction that asks the plate to zoom
    /// *out* (`−30`, the stream the audit sent) unless `step` says otherwise.
    fn notch(events: &mut Vec<egui::Event>, step: f32) {
        events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, step),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::default(),
        });
    }

    fn pin(place: &str, lat: f64, lon: f64, count: i64, article: i64) -> Pin {
        Pin {
            place: place.to_owned(),
            lat,
            lon,
            count,
            article,
        }
    }

    /// **Defect 2, measured**: six +20 px pointer steps over the plate moved the picture
    /// +28.9, +34.3, +35.9, +36.3, +36.5, +36.5 — a gain of 1.83 px per pointer px — and then
    /// coasted another 54 px, 263 px for a 120 px drag. Both halves had one cause: the press
    /// recorded a velocity *and* the frame integrated it, so every pointer point was applied
    /// twice and the second copy kept going after the hand stopped. Now the picture follows
    /// the pointer exactly while the button is down (gain 1.00) and the flick is only spent
    /// after the release.
    #[test]
    fn a_drag_is_the_pointers_own_distance_and_coasts_only_after_the_release() {
        let ctx = context();
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        // frame zero seats the camera in the box; a plate at the fit has no slack to drag —
        // `geo` refuses to show air — so the drag happens at ×3, where it has room
        frame(&ctx, 0, Vec::new(), |ui| {
            view.show(ui, &[], CARD.1);
        });
        let mut camera = view.camera();
        camera.zoom_at(Point::new(0.0, 0.0), 3.0);
        view.camera = camera;
        view.motion = Motion::new(camera);

        let start = Pos2::new(450.0, 240.0);
        let mut index = 1;
        frame(&ctx, index, press(start), |ui| {
            view.show(ui, &[], CARD.1);
        });
        index += 1;

        let mut at = start;
        let mut gain = Vec::new();
        let mut was = view.camera().offset().x;
        for _ in 0..6 {
            at.x += 20.0;
            frame(&ctx, index, vec![egui::Event::PointerMoved(at)], |ui| {
                view.show(ui, &[], CARD.1);
            });
            index += 1;
            let now = view.camera().offset().x;
            gain.push((now - was) / 20.0);
            was = now;
        }
        for (step, gain) in gain.iter().enumerate() {
            assert!(
                (gain - 1.0).abs() <= 0.02,
                "pointer step {} moved the picture {:.1} px for its 20 (gain {gain:.2}); a drag \
                 is the pointer's own distance",
                step + 1,
                gain * 20.0
            );
        }
        assert!(
            view.camera().offset().x - camera.offset().x >= 119.0,
            "the six steps moved the plate {:.1} px for 120",
            view.camera().offset().x - camera.offset().x
        );
        println!(
            "drag gain per +20 px pointer step: {}",
            gain.iter()
                .map(|gain| format!("{gain:.2}"))
                .collect::<Vec<_>>()
                .join(" ")
        );

        // nothing coasts while the button is down: the frame after the last step, with the
        // pointer still, must not move the picture at all
        let held = view.camera().offset().x;
        frame(&ctx, index, Vec::new(), |ui| {
            view.show(ui, &[], CARD.1);
        });
        index += 1;
        assert!(
            (view.camera().offset().x - held).abs() < 0.01,
            "the plate moved {:.2} px under a still pointer and a held button",
            view.camera().offset().x - held
        );

        // and the release is where a flick begins
        frame(&ctx, index, release(at), |ui| {
            view.show(ui, &[], CARD.1);
        });
        index += 1;
        assert!(view.motion.is_moving(), "the release left no flick to coast with");
        let mut coast = 0.0;
        let mut last = view.camera().offset().x;
        let mut frames = 0;
        while view.motion.is_moving() && frames < 400 {
            frame(&ctx, index, Vec::new(), |ui| {
                view.show(ui, &[], CARD.1);
            });
            index += 1;
            frames += 1;
            let now = view.camera().offset().x;
            coast += now - last;
            last = now;
        }
        println!("coast after the release: {coast:.1} px over {frames} frames");
        assert!(coast > 20.0, "the flick coasted {coast} px");
        assert!(frames < 200, "a coast of {frames} frames is a drift, not a flick");
    }

    /// **Defect 3, measured**: eight `Event::Zoom(1.4)` landed on ×13.0 at 60 Hz and ×12.1 at
    /// 120 Hz, because the ramp was `pending.clamp(±0.35)` per *frame* — a frame is not a unit
    /// of time. The same stream now applies the same zoom in the same tenth of a second
    /// whatever the display's rate.
    #[test]
    fn a_zoom_stream_takes_the_same_tenth_of_a_second_at_any_refresh_rate() {
        let mut landed = Vec::new();
        for hz in [30.0_f32, 60.0, 120.0, 240.0] {
            let start = Camera::new(CARD);
            let mut motion = Motion::new(start);
            let mut camera = start;
            for _ in 0..8 {
                motion.zoom_by(1.4_f32.ln(), Point::new(0.0, 0.0));
            }
            // a tenth of a second, however many frames that is, and the same *queue*: the
            // stream has already arrived, so what is measured is the ramp and not the input
            for _ in 0..(0.1 * hz).round() as usize {
                camera = motion.step(camera, 1.0 / hz).0;
            }
            landed.push((camera.zoom() / start.zoom()).ln());
        }
        let low = landed.iter().copied().fold(f32::INFINITY, f32::min);
        let high = landed.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        println!(
            "a tenth of a second of the same burst: 30 Hz ×{:.3}, 60 Hz ×{:.3}, 120 Hz ×{:.3}, \
             240 Hz ×{:.3}",
            landed[0].exp(),
            landed[1].exp(),
            landed[2].exp(),
            landed[3].exp()
        );
        assert!(low > 0.5, "the burst barely zoomed: {low} log");
        assert!(
            (high - low) <= high * 0.01,
            "the same stream landed {:.3} log apart across 30–240 Hz (×{:.2} … ×{:.2}): the \
             ramp is still counted in frames",
            high - low,
            low.exp(),
            high.exp()
        );
    }

    /// **Defect 4, measured**: two hundred pinch frames of 0.02 pt are 3.98 pt of ctrl-scroll,
    /// which egui makes `e^(3.98/200)` = ×1.0201. The plate's `(pinch - 1.0).abs() > 1e-4`
    /// dropped every one of them — each frame is 0.02 pt, a factor of 1.0001, exactly the
    /// threshold — so the whole pinch was ×1.0. A slow pinch is the one a reader uses to get a
    /// place *exactly* under the pointer, so it is the one that must not be thrown away.
    #[test]
    fn a_slow_pinch_is_spent_and_not_thrown_away() {
        // The audit's stream, and a gentler one beside it: 200 pinch frames of 0.02 pt are
        // 3.98 pt of ctrl-scroll (×1.0201), 200 of 0.01 pt are 1.99 pt (×1.0100). egui makes
        // one point of ctrl-scroll `e^(1/200)`, so the gentler frame is a factor of 1.00005 —
        // half the plate's old `1e-4` threshold, and every one of those was dropped. A slow
        // pinch is the gesture a reader uses to put a place exactly under the pointer, so it
        // is the one that must not be thrown away.
        for (per_frame, asked) in [(0.02_f32, 3.98_f32), (0.01, 1.99)] {
            let start = Camera::new(CARD);
            let mut motion = Motion::new(start);
            let mut camera = start;
            let pinch = (per_frame / 200.0).exp();
            for _ in 0..200 {
                motion.pinch(pinch, Point::new(0.0, 0.0));
            }
            let mut frames = 0;
            while motion.is_moving() {
                camera = motion.step(camera, 1.0 / 60.0).0;
                frames += 1;
                assert!(frames < 240, "the pinch never landed");
            }
            let wanted = (asked / 200.0).exp();
            let landed = camera.zoom() / start.zoom();
            println!(
                "200 pinch frames of {per_frame} pt: ×{landed:.4}, asked for ×{wanted:.4}, in \
                 {frames} frames"
            );
            // In *log* units, and as a fraction of the gesture: a tolerance on the factor
            // itself would be a percent of a percent on the gentler stream, and would pass a
            // plate that threw the whole pinch away.
            assert!(
                (landed.ln() - wanted.ln()).abs() <= wanted.ln() * 0.02,
                "200 frames of {per_frame} pt of pinch ({asked} pt) landed on ×{landed:.4} \
                 instead of ×{wanted:.4}"
            );
        }
    }

    /// **Defect 1, measured**: six −30 pt notches over the page's text scrolled it 178 px; the
    /// same six over the plate scrolled it 0 px *and* zoomed it 0, because the plate zeroed
    /// the wheel before it knew whether the camera had anywhere to go — and egui reads that
    /// field only after the content, so the page could never recover it. At the floor the
    /// wheel is now the page's, to the pixel; above it the plate takes the whole reading and
    /// the page holds still.
    #[test]
    fn at_the_floor_the_wheel_over_the_plate_is_the_pages() {
        // −30 pt asks to zoom *out*, which a plate at the fit cannot do: this is the audit's
        // own stream, and the one that used to vanish
        let (page, page_zoom) = wheel_case(Pos2::new(450.0, 560.0), 1.0, -30.0);
        let (over_plate, plate_zoom) = wheel_case(Pos2::new(450.0, 240.0), 1.0, -30.0);
        println!("six notches: over text {page:.1} px of page, over the plate {over_plate:.1} px");
        assert!(page > 100.0, "the text case scrolled only {page} px: the stream is wrong");
        assert!(
            (over_plate - page).abs() <= 1.0,
            "at the floor a wheel over the plate scrolled the page {over_plate:.1} px where the \
             same wheel over text scrolled {page:.1}"
        );
        assert_eq!(page_zoom, 1.0, "the page case zoomed the plate");
        assert_eq!(plate_zoom, 1.0, "a plate at its floor cannot zoom out");

        // +30 pt at ×1.5 asks to zoom in, which the plate has room for: the reading is the
        // plate's and the page must not move at all
        let (still, deeper) = wheel_case(Pos2::new(450.0, 240.0), 1.5, 30.0);
        println!("above the floor: {still:.2} px of page, ×{deeper:.3} of plate");
        assert!(
            still.abs() <= 0.5,
            "above the floor the plate left {still:.2} px of the wheel to the page"
        );
        assert!(deeper > 1.5, "the plate did not zoom in: ×{deeper}");
    }

    /// One run of six 30 pt notches over a plate at `factor` × the fit, with the pointer
    /// parked at `at`. Answers the page's scroll offset and the plate's zoom afterwards.
    fn wheel_case(at: Pos2, factor: f32, step: f32) -> (f32, f32) {
        let ctx = context();
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        let mut index = 0;
        let mut offset = page_frame(&ctx, index, vec![egui::Event::PointerMoved(at)], &mut view);
        index += 1;
        if factor == 1.0 {
            // A camera that kept the scale it was given by a wider frame sits a percent above the
            // floor the page's scroll bar leaves it (`Camera::resize` keeps the zoom), and a plate
            // a percent above its floor legitimately spends the first notch of a zoom-out. The
            // case under test is a plate *at* its floor, so it is put there — twice, because the
            // box the plate gets is only final once the scroll bar has taken its width.
            for _ in 0..2 {
                let plate = view.plate_rect().expect("the first frame drew no plate");
                let camera = Camera::new((plate.width(), plate.height()));
                view.camera = camera;
                view.motion = Motion::new(camera);
                offset = page_frame(&ctx, index, Vec::new(), &mut view);
                index += 1;
            }
        }
        if factor != 1.0 {
            let mut camera = view.camera();
            camera.zoom_at(Point::new(0.0, 0.0), factor);
            view.camera = camera;
            view.motion = Motion::new(camera);
            offset = page_frame(&ctx, index, Vec::new(), &mut view);
            index += 1;
        }
        for _ in 0..6 {
            let mut events = Vec::new();
            notch(&mut events, step);
            offset = page_frame(&ctx, index, events, &mut view);
            index += 1;
        }
        // the frames after the last notch, while egui's wheel tween finishes arriving
        for _ in 0..10 {
            offset = page_frame(&ctx, index, Vec::new(), &mut view);
            index += 1;
        }
        (offset, view.camera().zoom())
    }

    /// **Defect 5, measured**: Esc did nothing on the plate. A click on a dot opens a story on
    /// another page, and a reader with no mouse had no way to say "not that one" — the key
    /// block had arrows, `+`, `−` and `0`. Esc now clears the picked dot and leaves the plate
    /// exactly where it was, and the press that picks the dot is also what gives the plate the
    /// focus the rest of its keys need.
    #[test]
    fn escape_clears_the_picked_pin_and_keeps_the_page() {
        let ctx = context();
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        let pins = vec![pin("Kinshasa", -4.3, 15.3, 3, 11)];
        let mut answered = None;
        frame(&ctx, 0, Vec::new(), |ui| {
            answered = view.show(ui, &pins, CARD.1);
        });
        assert!(answered.is_none(), "a frame with no pointer answered a pin");

        // the dot, at the pixel the projection puts it on, inside the window's own plate
        let camera = view.camera();
        let point = camera.project(Geo::new(15.3, -4.3));
        let dot = window_plate().center() + Vec2::new(point.x, point.y);
        let mut index = 1;
        frame(&ctx, index, press(dot), |ui| {
            view.show(ui, &pins, CARD.1);
        });
        index += 1;
        frame(&ctx, index, release(dot), |ui| {
            answered = view.show(ui, &pins, CARD.1);
        });
        index += 1;
        assert_eq!(answered, Some(0), "the click did not land on the dot at {dot:?}");
        assert_eq!(view.selected(), Some(0), "the picked dot is not remembered");
        let picked = view.camera().zoom();

        // Esc: the key reaches a plate that took focus from the press
        let mut escape = Vec::new();
        escape.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        answered = None;
        frame(&ctx, index, escape, |ui| {
            answered = view.show(ui, &pins, CARD.1);
        });
        assert_eq!(view.selected(), None, "Esc left the dot picked");
        assert_eq!(answered, None, "Esc answered a pin, which would open its story");
        assert_eq!(view.camera().zoom(), picked, "Esc moved the page it was told to keep");
    }

    /// The caption's promise, pinned against the clamp it describes: at the fit the plate is
    /// the box, so panning has nowhere to go, and the line has to say so. The phrase
    /// `drag to pan, scroll to zoom` stays whole in both readings because `frame_cost.rs`
    /// reads it to tell a painted plate from an unpainted one.
    #[test]
    fn the_caption_promises_only_what_the_clamp_allows() {
        let fit = caption(1.0, 5, 1200);
        println!("{fit}");
        assert_eq!(
            fit,
            "zoom ×1.0 · 5 places · 1 200 segments · the whole world is the plate · drag to pan, \
             scroll to zoom once you zoom in"
        );
        let deep = caption(2.5, 5, 1200);
        println!("{deep}");
        assert_eq!(
            deep,
            "zoom ×2.5 · 5 places · 1 200 segments · drag to pan, scroll to zoom",
            "above the fit there is slack, so the caveat is gone"
        );
        assert!(
            fit.contains("drag to pan, scroll to zoom"),
            "the frame-cost sentinel went missing from the fit's reading"
        );
    }

    /// The audit's measurement, from this side of the border: at the opening zoom the plate is
    /// the box, so a drag can only spend the slack between the plate and the box — and on a box
    /// as wide as the world's 2:1 there is none at all. Six +20 px steps therefore move the
    /// plate 0.0 px there (the audit's number), and on a box taller than 2:1 they move it by
    /// that axis's slack and no further. `geo` is right to refuse the rest: air beside the
    /// world is the worse defect. This is what the caption above is about.
    #[test]
    fn a_drag_at_the_fit_moves_the_plate_by_its_slack_and_no_further() {
        // 1 000 × 470 is wider than the world's 2:1, so the width is the binding axis and the
        // horizontal slack is exactly zero — the audit's case, without a frame in the way
        let mut wide = Camera::new((1000.0, 470.0));
        wide.pan((120.0, 0.0));
        println!("a 120 px drag at the fit on a 1 000 × 470 box: {:.2} px", wide.offset().x);
        assert_eq!(
            wide.offset().x,
            0.0,
            "a horizontal drag at the fit on a 2:1-or-wider box must move nothing"
        );

        // and through the frame, on the window's own plate: the drag is spent on the slack and
        // then stops, however much further the pointer goes
        let ctx = context();
        let mut view = MapView::new(Path::new("/nonexistent/atlas-no-outlines.geojson"));
        frame(&ctx, 0, Vec::new(), |ui| {
            view.show(ui, &[], CARD.1);
        });
        assert!(
            (view.camera().zoom() - 1.0).abs() < 1e-4,
            "the plate did not open at the fit"
        );
        let start = Pos2::new(450.0, 240.0);
        let mut index = 1;
        frame(&ctx, index, press(start), |ui| {
            view.show(ui, &[], CARD.1);
        });
        index += 1;
        let mut at = start;
        let mut last = view.camera().offset().x;
        for _ in 0..12 {
            at.x += 20.0;
            frame(&ctx, index, vec![egui::Event::PointerMoved(at)], |ui| {
                view.show(ui, &[], CARD.1);
            });
            index += 1;
            let now = view.camera().offset().x;
            assert!(
                (now - last).abs() <= 20.0 + 1e-2,
                "one step of 20 px moved the plate {:.2} px",
                now - last
            );
            last = now;
        }
        println!(
            "a 240 px drag at the fit on the 900 × 470 window plate moved it {:.1} px (its slack)",
            view.camera().offset().x
        );
        assert!(
            view.camera().offset().x.abs() < 40.0,
            "a 240 px drag at the fit moved the plate {:.2} px: more than the plate's own slack",
            view.camera().offset().x
        );
    }


    // --------------------------------------------------------------- clicks on land

    /// An inland point of `iso`, discovered by scanning a lattice inside the rings the world
    /// says are that country: the test may not invent a coordinate and then complain that the
    /// file disagrees, so the point is found here and asserted where it is used.
    fn inland(world: &World, iso: &str) -> Option<Geo> {
        for ring in &world.rings {
            if &*ring.iso != iso {
                continue;
            }
            const STEPS: usize = 24;
            for row in 0..=STEPS {
                let lat = ring.bounds.south
                    + (ring.bounds.north - ring.bounds.south) * (row as f32 / STEPS as f32);
                for column in 0..=STEPS {
                    let lon = ring.bounds.west
                        + (ring.bounds.east - ring.bounds.west) * (column as f32 / STEPS as f32);
                    let at = Geo::new(lon, lat);
                    if world.country_at(at) == Some(iso) {
                        return Some(at);
                    }
                }
            }
        }
        None
    }

    /// The point the plate draws a country's ring at is **inside** that country.
    ///
    /// This is the whole justification for a second position: a story has one coordinate (its
    /// city) and is drawn there; its country's ring is drawn at Natural Earth's label point,
    /// which is a cartographic point chosen by hand to sit on land and inside the border. If
    /// the file's point were a centroid, an archipelago's ring would sit in the sea — so the
    /// claim is tested rather than trusted: every code the file draws is asked for its point,
    /// and the point is fed back through the country hit test.
    #[test]
    fn the_label_point_of_a_country_is_inside_that_country() {
        let world = world();
        let text = std::fs::read_to_string(crate::assets::geojson())
            .expect("the vendored outlines are readable");
        let root: serde_json::Value = serde_json::from_str(&text).expect("the file is JSON");
        let features = root
            .get("features")
            .and_then(|value| value.as_array())
            .expect("a FeatureCollection");
        let mut checked = 0;
        let mut misses: Vec<(String, Geo, Option<String>)> = Vec::new();
        for feature in features {
            let properties = feature.get("properties").unwrap_or(&serde_json::Value::Null);
            let iso = code_of(properties);
            if iso.is_empty() {
                continue;
            }
            let Some(at) = world.label_point(&iso) else {
                misses.push((iso, Geo::new(0.0, 0.0), None));
                continue;
            };
            checked += 1;
            let inside = world.country_at(at).map(str::to_string);
            if inside.as_deref() != Some(iso.as_str()) {
                misses.push((iso, at, inside));
            }
        }
        println!(
            "{checked} label points, {} of them not inside their own country by the plate's hit \
             test: {misses:?}",
            misses.len()
        );
        assert!(checked > 200, "only {checked} countries carry a label point");
        // Measured: 12 of 242, and every one of them is an enclave (VAT and SMR inside Italy's
        // ring, LSO inside South Africa's), an island smaller than this hit test's resolution
        // (TTO, STP, VCT, ATG, MAC), New Zealand (the file's point is in Cook Strait) or a
        // territory the file draws no land for (SGS, IOT). A ring still answers its own code:
        // the press is answered by distance to the ring, not by the land under it.
        assert!(
            misses.len() <= 12,
            "{} label points are not inside their own country: {misses:?}",
            misses.len()
        );
        // The three the window actually draws at the opening view, and the reported case.
        for iso in ["DEU", "USA", "CHN", "UKR", "FRA"] {
            let at = world.label_point(iso).unwrap_or_else(|| panic!("{iso} has no label point"));
            assert_eq!(
                world.country_at(at),
                Some(iso),
                "{iso}'s label point {at:?} answered {:?}",
                world.country_at(at)
            );
        }
    }

    /// A click on a country's land answers that country, and open water answers nothing.
    ///
    /// The reader's complaint: clicking Germany did nothing at all. `country_at` is the answer,
    /// and the code it gives is the one `Archive::country_stories` reads — alpha-3, the
    /// archive's own alphabet. Six continents, because a hemisphere that works is not a hit
    /// test.
    #[test]
    fn a_click_on_land_names_the_country_and_open_water_names_nothing() {
        let world = World::load(&crate::assets::geojson());
        assert!(world.lines() > 500, "the vendored outlines did not load");
        let mut found = 0;
        for iso in ["DEU", "USA", "BRA", "JPN", "ZAF", "AUS"] {
            let Some(at) = inland(&world, iso) else {
                panic!("no inland point of {iso} found in the file: {iso} is not a country it draws")
            };
            assert_eq!(
                world.country_at(at),
                Some(iso),
                "{iso} at {at:?} answered {:?}",
                world.country_at(at)
            );
            found += 1;
        }
        assert_eq!(found, 6);
        // Open water: the middle of the South Pacific, and a point north of everything.
        assert_eq!(world.country_at(Geo::new(-140.0, -40.0)), None);
        assert_eq!(world.country_at(Geo::new(0.0, 89.0)), None);
        // A coordinate that is not a number is not a place either, and it must not panic.
        assert_eq!(world.country_at(Geo::new(f32::NAN, 0.0)), None);
        assert_eq!(world.country_at(Geo::new(0.0, f32::INFINITY)), None);
    }

    /// The hit test through the camera: a plate pixel over Germany is Germany, at two zooms.
    ///
    /// This is the call `MapView::show` makes on a press, so it is the one that has to work at
    /// the scale the reader is looking at — the inverse projection is not a separate answer
    /// from the one the coast is drawn with.
    #[test]
    fn a_pixel_over_a_country_is_that_country_at_every_zoom() {
        let world = World::load(&crate::assets::geojson());
        let berlin = Geo::new(13.4, 52.5);
        for factor in [1.0_f32, 2.5, 9.0] {
            let camera = centred(berlin, factor);
            let point = camera.project(berlin);
            assert_eq!(
                world.country_at_plate(&camera, point),
                Some("DEU"),
                "at ×{factor} the pixel that is Berlin answered {:?}",
                world.country_at_plate(&camera, point)
            );
        }
        // The inverse is the projection's own: a pixel far off the coast is water.
        let camera = Camera::new(CARD);
        let mid_atlantic = camera.project(Geo::new(-30.0, 20.0));
        assert_eq!(world.country_at_plate(&camera, mid_atlantic), None);
    }

    // ------------------------------------------------------------- clicks on a dot

    /// A dot that merges places names every one of them, heaviest first.
    ///
    /// The defect this pins: `Cluster` carried only `rep`, so the busiest dot of the shipped
    /// archive (3 places, 99 stories) opened one story and hid the rest, and no zoom could
    /// separate them — city and country share a coordinate.
    #[test]
    fn a_merged_dot_names_every_place_it_stands_for() {
        let pins = vec![
            Pin { place: "China".into(), lat: 35.0, lon: 105.0, count: 88, article: 7 },
            Pin { place: "Beijing".into(), lat: 35.1, lon: 105.1, count: 11, article: 8 },
            Pin { place: "Oslo".into(), lat: 59.9, lon: 10.7, count: 6, article: 9 },
        ];
        let rect = plate();
        let camera = Camera::new(CARD);
        let dots = cluster(&pins, |pin| screen(&camera, Geo::new(pin.lon as f32, pin.lat as f32)), rect, CLUSTER_PX);
        let merged = dots
            .iter()
            .find(|dot| dot.places > 1)
            .expect("two pins 0.1° apart are one dot at the opening zoom");
        assert_eq!(merged.places, 2);
        assert_eq!(merged.stories, 99);
        assert_eq!(merged.rep, 0, "the heaviest pin represents the dot");
        assert_eq!(merged.members, vec![0, 1], "the dot names both places");
        let lines = card_lines(merged, &pins, &[]);
        assert!(
            lines.iter().any(|line| line.contains("China") && line.contains("Beijing")),
            "the card of a merged dot does not name its places: {lines:?}"
        );
        // A dot of one place carries itself, so `members` is never empty and `length == 1`
        // means "one place" to every caller.
        let single = dots.iter().find(|dot| dot.places == 1).expect("Oslo is its own dot");
        assert_eq!(single.members, vec![single.rep]);
    }

    /// The card says the story a press opens, and says so differently when the story is gone.
    #[test]
    fn the_card_names_the_story_and_says_when_it_is_gone() {
        let live = Cluster {
            rep: 0,
            members: vec![0],
            at: Pos2::new(10.0, 10.0),
            places: 1,
            stories: 9,
            radius: 4.0,
        };
        let here = vec![Pin { place: "Kinshasa".into(), lat: -4.3, lon: 15.3, count: 9, article: 11 }];
        let gone = vec![Pin { place: "Kinshasa".into(), lat: -4.3, lon: 15.3, count: 9, article: 0 }];
        let with_id = card_lines(&live, &here, &[]);
        assert_eq!(with_id, vec!["opens story 11".to_string()]);
        let titled = card_lines(&live, &here, &["Chips leave the port".to_string()]);
        assert_eq!(
            titled,
            vec!["Chips leave the port".to_string()],
            "a headline the caller has is what the card says"
        );
        let without = card_lines(&live, &gone, &[]);
        assert_eq!(without.len(), 1);
        assert!(
            without[0].contains("left the archive"),
            "a dot whose story is gone paints no sentence of its own: {without:?}"
        );
        assert_ne!(with_id, without, "a dead door reads like a live one");
    }

    /// The keyboard walks the dots in the slice's order, and Enter opens the one it is on.
    #[test]
    fn the_walk_order_is_the_order_the_pins_came_in() {
        // The order is `Archive::pins`' own (`n DESC, label ASC`), which is the order this
        // slice is handed in: the plate never sorts, so the walk is stable across frames,
        // re-reads and runs.
        let pins = crate::data::Archive::open(&crate::assets::archive())
            .map(|archive| archive.pins(72, 40))
            .unwrap_or_default();
        if pins.len() < 3 {
            eprintln!("skipped: the archive holds {} located places", pins.len());
            return;
        }
        let mut walk: Vec<usize> = Vec::new();
        let mut selected: Option<usize> = None;
        for _ in 0..pins.len() {
            let from = selected.map(|index| index as i32).unwrap_or(-1);
            let next = (from + 1).rem_euclid(pins.len() as i32) as usize;
            selected = Some(next);
            walk.push(next);
        }
        // A full turn visits every dot once and comes back to the first: a key that stuck, or
        // one that skipped, would not.
        let mut sorted = walk.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), pins.len(), "the walk visited {} of {} dots", sorted.len(), pins.len());
        assert_eq!(walk.first(), Some(&0), "the first step picks the first dot");
    }

}
