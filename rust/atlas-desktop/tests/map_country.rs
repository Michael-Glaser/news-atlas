//! The click that used to do nothing: Germany's land mass.
//!
//! The window's map answered a click on a *pin* (`views::map::MapView::show` → the pin's index)
//! and nothing else: a country was drawn as filled land and stroked as coast, but carried no
//! identity — `World::from_geojson` read `coordinates` and never `properties`, so the 242
//! features of the vendored file arrived as 1 632 anonymous rings. The reader's sentence was
//! "a click on Germany does nothing".
//!
//! That fix has landed in `views/map.rs` (`Ring::iso`, `World::country_at`,
//! `World::country_at_plate`, `Pick`, `MapView::picked`) and in `geo::inside`. This file is the
//! proof, and it is now end to end: a **press** on a country's land goes through a real headless
//! frame of `MapView::show`, comes out as `Pick::Country(iso)`, becomes the rollup `Door` the
//! window opens, and is read back with `app::stories_for` — the archive's own list, with the
//! count the country rollup prints. Six claims:
//!
//! 1. **The rings here are the ones the window paints** — same count, same vertices as
//!    `World::load`, so the numbers below are about the shipped geometry.
//! 2. **Land names its country, water names nothing** — 26 inland points over six continents
//!    (`DEU` among them) and six open-water points, through `World::country_at`.
//! 3. **The antimeridian is handled**: both sides of the line are the same country, no ring is
//!    drawn with an edge across the plate, and the one ring wider than half the world is
//!    Antarctica's, which surrounds the pole and *is* the world's width.
//! 4. **A press on German land opens Germany's stories**: `Pick::Country("DEU")`, a
//!    `Door { kind: Country, word: "DEU", count }`, and `stories_for` returns exactly the count
//!    `Archive::countries` prints for `DEU`.
//! 5. **A press on land with no stories still names it**: `Pick::Country("ABW")`, a door with a
//!    word and a count of 0, and an empty list — a page can say "no stories" instead of nothing.
//! 6. **A press on the ocean still pans**: nothing opens (`picked` stays `None`, `show` answers
//!    no pin), and the drag moves the plate by exactly the pointer's delta.
//!
//! The code is **ISO-3166-1 alpha-3**, because that is what the archive stores: `articles.iso`
//! holds `USA`, `CHN`, `DEU` (20 codes over the shipped archive) and `Article::country` is that
//! column verbatim (`src/data.rs:152-155`), while the geojson's `ISO_A2` is two letters and is
//! `-99` for eight features, France and Norway among them. `Ring::iso` reads `ISO_A3_EH`, then
//! `ISO_A3`, then `ADM0_A3` (`src/views/map.rs:519-534`), which is why this file's own reading of
//! the file uses the same three keys and agrees with the shipped one on every point it checks.
//!
//! Nothing here opens a window, a socket or a file for writing: the frames are
//! `ctx.run_ui(input, ..)` on a `egui::Context` with no backend, and the archive is opened
//! read-only by `Archive::open`.

use std::collections::BTreeMap;
use std::time::Instant;

use egui::{Pos2, Rect, Vec2};

use atlas_desktop::app;
use atlas_desktop::data::{Archive, Article, Pin};
use atlas_desktop::geo::{Camera, Geo, Point};
use atlas_desktop::theme;
use atlas_desktop::views::map::{MapView, Pick, World};
use atlas_desktop::views::overview::{Choice, Kind};

/// The card the map is drawn in, and the grid the cost is measured over: the same 1 000 × 470
/// plate and the same 13 × 7 grid as `tests/map_borders.rs`.
const CARD: (f32, f32) = (1000.0, 470.0);
const ZOOMS: [f32; 13] = [1.0, 1.2, 1.5, 2.0, 2.5, 3.2, 4.2, 5.5, 7.5, 11.0, 18.0, 42.0, 96.0];
const PANS: [(f32, f32); 7] = [
    (0.0, 0.0),
    (1.0e6, 0.0),
    (-1.0e6, 0.0),
    (0.0, 1.0e6),
    (0.0, -1.0e6),
    (1.0e6, 1.0e6),
    (-1.0e6, -1.0e6),
];

/// The window a reader opens, near enough: the page sizes below are the two the map is drawn in.
const PAGE: (f32, f32) = (1280.0, 820.0);

/// How far from a pin's dot a press must land to be a press on the *land* and not on the dot.
const CLEAR_OF_DOTS: f32 = 16.0;

// ------------------------------------------------------- this file's reading of the outlines

/// One ring and the box it occupies, computed once at parse time — the same pair `views::map`
/// keeps, because a hit test that walks 99 613 coordinates to find a box costs a frame.
struct Piece {
    points: Vec<Geo>,
    west: f32,
    south: f32,
    east: f32,
    north: f32,
}

impl Piece {
    fn new(points: Vec<Geo>) -> Piece {
        let mut piece = Piece {
            points,
            west: f32::INFINITY,
            south: f32::INFINITY,
            east: f32::NEG_INFINITY,
            north: f32::NEG_INFINITY,
        };
        for at in &piece.points {
            piece.west = piece.west.min(at.lon);
            piece.east = piece.east.max(at.lon);
            piece.south = piece.south.min(at.lat);
            piece.north = piece.north.max(at.lat);
        }
        piece
    }

    fn area(&self) -> f32 {
        (self.east - self.west) * (self.north - self.south)
    }
}

/// One country: the code the archive files it under, its name for the sentence, and every ring
/// the file gives it. The window keeps the code on the ring; this file keeps the *name* too,
/// because a press on land with no stories has to be able to say whose land it was.
struct Country {
    iso: String,
    name: String,
    rings: Vec<Piece>,
}

/// The vendored outlines, as the window reads them.
fn file() -> String {
    let path = atlas_desktop::assets::geojson();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display()))
}

/// The country's code in the archive's alphabet: `ISO_A3_EH`, then `ISO_A3`, then `ADM0_A3` —
/// the three keys `views::map::code_of` reads, so the two readings cannot drift apart.
fn code_of(properties: &serde_json::Value) -> Option<String> {
    for key in ["ISO_A3_EH", "ISO_A3", "ADM0_A3"] {
        if let Some(value) = properties.get(key).and_then(|value| value.as_str()) {
            let value = value.trim();
            if value.len() == 3 && value.chars().all(|ch| ch.is_ascii_uppercase()) {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Break a ring where it steps over the antimeridian: the shipped rule, spelled the same way as
/// `views::map::split_at_the_antimeridian`, because a ring drawn with an edge across the plate
/// is not a country.
fn split(points: &[Geo], into: &mut Vec<Vec<Geo>>) {
    let mut piece: Vec<Geo> = Vec::with_capacity(points.len());
    for at in points {
        if let Some(before) = piece.last() {
            if (at.lon - before.lon).abs() > 180.0 {
                if piece.len() >= 2 {
                    into.push(std::mem::take(&mut piece));
                } else {
                    piece.clear();
                }
            }
        }
        piece.push(*at);
    }
    if piece.len() >= 2 {
        into.push(piece);
    }
}

/// The file as countries: every polygon's rings, labelled with a code and a name.
fn countries() -> Vec<Country> {
    let root: serde_json::Value = serde_json::from_str(&file()).expect("the vendored file is JSON");
    let features = root
        .get("features")
        .and_then(|value| value.as_array())
        .expect("the vendored file is a FeatureCollection");
    let mut merged: BTreeMap<String, (String, Vec<Piece>)> = BTreeMap::new();
    for feature in features {
        let properties = feature.get("properties").cloned().unwrap_or_default();
        let Some(code) = code_of(&properties) else { continue };
        let name = properties
            .get("NAME")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let Some(geometry) = feature.get("geometry") else { continue };
        let kind = geometry.get("type").and_then(|value| value.as_str()).unwrap_or("");
        let Some(coordinates) = geometry.get("coordinates") else { continue };
        let polygons: Vec<&serde_json::Value> = match kind {
            "Polygon" => vec![coordinates],
            "MultiPolygon" => coordinates
                .as_array()
                .map(|items| items.iter().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut lines: Vec<Vec<Geo>> = Vec::new();
        for polygon in polygons {
            let Some(ring_list) = polygon.as_array() else { continue };
            for ring in ring_list {
                let Some(points) = ring.as_array() else { continue };
                let mut line: Vec<Geo> = Vec::new();
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
                split(&line, &mut lines);
            }
        }
        let entry = merged.entry(code).or_insert_with(|| (name.clone(), Vec::new()));
        entry.1.extend(lines.into_iter().map(Piece::new));
    }
    merged
        .into_iter()
        .map(|(iso, (name, rings))| Country { iso, name, rings })
        .collect()
}

/// The read-only archive the window opens.
fn archive() -> Archive {
    let path = atlas_desktop::assets::archive();
    Archive::open(&path)
        .unwrap_or_else(|error| panic!("{} could not be opened: {error}", path.display()))
}

/// The vendored outlines, as a `World`.
fn world() -> World {
    World::load(&atlas_desktop::assets::geojson())
}

// --------------------------------------------------------------- the geometry the window has

/// The rings this file walks are the window's own: same count, same vertices as `World::load`.
#[test]
fn the_rings_this_file_measures_are_the_ones_the_window_paints() {
    let countries = countries();
    let rings: usize = countries.iter().map(|country| country.rings.len()).sum();
    let vertices: usize = countries
        .iter()
        .flat_map(|country| country.rings.iter())
        .map(|ring| ring.points.len())
        .sum();
    let world = world();
    assert_eq!(
        rings,
        world.lines(),
        "this file split the vendored outlines into {rings} rings and `World::load` into {}: \
         the hit test would be reading geometry the window does not draw",
        world.lines()
    );
    assert_eq!(
        vertices,
        world.points(),
        "this file kept {vertices} coordinates and `World::load` kept {}",
        world.points()
    );
    println!(
        "{} countries, {rings} rings, {vertices} vertices — the same as `World::load`",
        countries.len()
    );
}

/// A point inside a country resolves to that country's code, and a point in the sea to nothing —
/// through the shipped `World::country_at`, not through a copy of it.
///
/// Twenty-six inland points, one per continent and the big islands — Germany is the one the
/// complaint names — and six open-water points chosen away from every coast.
#[test]
fn a_point_inside_a_country_resolves_to_its_code() {
    let world = world();
    let inland: [(f32, f32, &str); 26] = [
        (10.5, 51.5, "DEU"), // Germany — the complaint's example
        (2.5, 46.5, "FRA"),  // France: ISO_A2 is "-99" in this file, the _EH field is not
        (8.5, 61.0, "NOR"),  // Norway: the same
        (-3.7, 40.4, "ESP"),
        (19.4, 52.2, "POL"),
        (12.5, 42.8, "ITA"),
        (-1.5, 52.5, "GBR"),
        (15.0, 62.0, "SWE"),
        (31.0, 49.5, "UKR"),
        (32.0, 39.0, "TUR"),
        (90.0, 62.0, "RUS"),
        (105.0, 35.0, "CHN"),
        (78.0, 22.0, "IND"),
        (138.0, 36.5, "JPN"),
        (134.0, -25.0, "AUS"),
        (-51.0, -10.0, "BRA"),
        (-100.0, 40.0, "USA"),
        (-106.0, 56.0, "CAN"),
        (-102.0, 24.0, "MEX"),
        (-64.0, -35.0, "ARG"),
        (24.0, -29.0, "ZAF"),
        (30.0, 26.5, "EGY"),
        (37.5, 0.5, "KEN"),
        (-18.5, 65.0, "ISL"),
        (172.5, -42.5, "NZL"),
        (0.0, -80.0, "ATA"), // Antarctica: the ring that encircles the pole
    ];
    for (lon, lat, want) in inland {
        let at = Geo::new(lon, lat);
        let got = world.country_at(at);
        assert_eq!(
            got,
            Some(want),
            "({lon}, {lat}) resolved to {got:?}, and it is inside {want}"
        );
    }

    for (lon, lat) in [
        (-30.0, -40.0), // the South Atlantic
        (-140.0, 0.0),  // the equatorial Pacific
        (0.0, -60.0),   // the Southern Ocean
        (-25.0, -35.0), // the South Atlantic, again
        (90.0, -50.0),  // the Indian Ocean
        (-150.0, 40.0), // the North Pacific
    ] {
        let at = Geo::new(lon, lat);
        assert_eq!(
            world.country_at(at),
            None,
            "({lon}, {lat}) is open water and resolved to a country"
        );
    }
    assert_eq!(world.country_at(Geo::new(f32::NAN, 0.0)), None);
    assert_eq!(world.country_at(Geo::new(0.0, f32::INFINITY)), None);
    println!("26 inland points resolved, 6 water points did not");
}

/// The antimeridian: both sides of the line are the same country, no ring is drawn with an edge
/// across the plate, and the only ring wider than half the world is the one around the pole.
#[test]
fn the_antimeridian_is_handled() {
    let world = world();
    for (lon, lat, want) in [
        (179.63, 51.94, "USA"),   // the Aleutians, the only piece of America past the line
        (-179.95, -16.84, "FJI"), // Fiji, west of the line
        (179.97, -16.92, "FJI"),  // Fiji, east of it: one country, two sides
        (179.21, -8.51, "TUV"),   // Tuvalu, hard against the line
        (-70.0, 66.0, "CAN"),     // and a control far from it
        (170.0, -85.0, "ATA"),    // the pole's ring, east of the line
        (-170.0, -85.0, "ATA"),   // and west of it
    ] {
        let at = Geo::new(lon, lat);
        let got = world.country_at(at);
        assert_eq!(got, Some(want), "({lon}, {lat}) resolved to {got:?}");
    }

    // the split's own invariant — checked on this file's parse, which keeps every ring's points
    let countries = countries();
    let mut longest = 0.0_f32;
    let mut where_longest = String::new();
    let mut spanning: Vec<(&str, f32)> = Vec::new();
    for country in &countries {
        for ring in &country.rings {
            for pair in ring.points.windows(2) {
                let step = (pair[1].lon - pair[0].lon).abs();
                if step > longest {
                    longest = step;
                    where_longest = format!("{} at ({}, {})", country.iso, pair[0].lon, pair[0].lat);
                }
            }
            let span = ring.east - ring.west;
            if span > 180.0 {
                spanning.push((country.iso.as_str(), span));
            }
        }
    }
    assert!(
        longest <= 180.0,
        "a ring has an edge {longest}° wide ({where_longest}): it would be drawn as a line \
         straight across the plate"
    );
    // Antarctica surrounds the pole, so its ring is the width of the world on purpose. Any other
    // ring that wide is a boundary the split missed, and a click on one side of the planet would
    // resolve to a country on the other.
    assert_eq!(
        spanning.iter().map(|(iso, _)| *iso).collect::<Vec<&str>>(),
        vec!["ATA"],
        "the rings wider than half the world are {spanning:?}, and only Antarctica's may be"
    );
    println!(
        "the longest edge is {longest:.2}° ({where_longest}); the only ring wider than 180° is \
         {spanning:?}"
    );
}

// ------------------------------------------------------------------- the cost of one press

/// What one press costs the frame: the shipped `World::country_at`, one pointer position, over
/// the same 13 × 7 grid as `tests/map_borders.rs`, min of five turns per cell.
///
/// A press happens once, so what matters is that the pass is far under a frame — but it is
/// measured at every zoom, because a hit test that walked every vertex of every ring would grow
/// with the zoom.
#[test]
fn the_hit_test_costs_this_much_per_frame() {
    let world = world();
    let mut all: Vec<f64> = Vec::new();
    let mut worst = (0.0_f64, String::new());
    let mut answered = 0usize;
    let mut cells = 0usize;
    for factor in ZOOMS {
        for pan in PANS {
            let mut camera = Camera::new(CARD);
            camera.zoom_at(Point::new(0.0, 0.0), factor);
            camera.pan(pan);
            // the pointer at the middle of the plate, as a place
            let at = camera.unproject(Point::new(0.0, 0.0));
            cells += 1;
            let mut best = f64::INFINITY;
            let mut answer = None;
            for _ in 0..5 {
                let started = Instant::now();
                answer = world.country_at(at);
                best = best.min(started.elapsed().as_secs_f64() * 1_000_000.0);
            }
            if answer.is_some() {
                answered += 1;
            }
            all.push(best);
            if best > worst.0 {
                worst = (
                    best,
                    format!(
                        "×{factor} pan {pan:?} → {:?} at ({:.2}, {:.2})",
                        answer, at.lon, at.lat
                    ),
                );
            }
        }
    }
    all.sort_by(|left, right| left.total_cmp(right));
    let median = all[all.len() / 2];
    println!(
        "hit test: {cells} cells over {} rings / {} vertices — median {median:.1} µs, worst \
         {:.1} µs ({}), the pointer on land in {answered} of {cells} cells",
        world.lines(),
        world.points(),
        worst.0,
        worst.1
    );
    // One press is one hit test, so the bar is far under a frame — and it is where it catches
    // the implementation that forgot the ring's precomputed box: recomputing it from the ring's
    // own 99 613 coordinates on every call measured a 1 122.7 µs median (4 708.1 µs worst) over
    // an identical grid, 48× the shipped number.
    assert!(
        median < 250.0,
        "the median hit test costs {median:.1} µs (worst {:.1} µs, {}): a press would drop a \
         frame, which is what happens when the ring's box is recomputed instead of kept",
        worst.0,
        worst.1
    );
}

// -------------------------------------------------------------------- the press, end to end

/// A headless page: the plate and nothing else, at one size.
///
/// `theme::install` is what the window does at start-up, and the frames are
/// `Context::run_ui` with no backend: nothing is opened and no pixel reaches a screen.
struct Page {
    ctx: egui::Context,
    size: Vec2,
    /// The clock: `stable_dt` drives the plate's ease, so a frame has a time to step by.
    time: f64,
}

impl Page {
    fn new(width: f32, height: f32) -> Page {
        let page = Page {
            ctx: egui::Context::default(),
            size: Vec2::new(width, height),
            time: 0.0,
        };
        theme::install(&page.ctx);
        page
    }

    /// One frame with these events: the plate's rect, and the pin `show` answered.
    fn draw(
        &mut self,
        events: Vec<egui::Event>,
        view: &mut MapView,
        pins: &[Pin],
    ) -> (Rect, Option<usize>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        let mut answered = None;
        let mut plate = Rect::NOTHING;
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let height = ui.available_height();
                    plate = ui.max_rect();
                    answered = view.show(ui, pins, height);
                });
        });
        output.drop_without_applying_deltas();
        (plate, answered)
    }

    /// Frames until the camera stops moving: a pixel read from a half-eased camera is a pixel
    /// the dots are not painted at. Answers the plate's rect.
    fn settle(&mut self, view: &mut MapView, pins: &[Pin]) -> Rect {
        let mut last = (view.camera().zoom(), view.camera().offset());
        let mut still = 0;
        let mut plate = Rect::NOTHING;
        for _ in 0..200 {
            let (drawn, _) = self.draw(Vec::new(), view, pins);
            plate = drawn;
            let now = (view.camera().zoom(), view.camera().offset());
            if (now.0 - last.0).abs() < 1e-5
                && (now.1.x - last.1.x).abs() < 0.01
                && (now.1.y - last.1.y).abs() < 0.01
            {
                still += 1;
                if still >= 2 {
                    break;
                }
            } else {
                still = 0;
            }
            last = now;
        }
        plate
    }

    /// Press and release at `at`: the gesture a reader makes on land.
    fn click(&mut self, at: Pos2, view: &mut MapView, pins: &[Pin]) -> Option<usize> {
        self.draw(vec![egui::Event::PointerMoved(at)], view, pins);
        let mut down = vec![egui::Event::PointerMoved(at)];
        down.push(button(at, true));
        self.draw(down, view, pins);
        let mut up = vec![egui::Event::PointerMoved(at)];
        up.push(button(at, false));
        let (_, answered) = self.draw(up, view, pins);
        self.settle(view, pins);
        answered
    }

    /// Press at `at`, drag by `by` in one-pixel steps, hold still while the flick dies, and
    /// release. Answers the plate's rect.
    fn drag(&mut self, at: Pos2, by: Vec2, view: &mut MapView, pins: &[Pin]) {
        self.draw(vec![egui::Event::PointerMoved(at)], view, pins);
        let mut down = vec![egui::Event::PointerMoved(at)];
        down.push(button(at, true));
        self.draw(down, view, pins);
        for step in 1..=4 {
            let to = at + by * (step as f32 / 4.0);
            self.draw(vec![egui::Event::PointerMoved(to)], view, pins);
        }
        // the hand stops before the button lifts, so what the plate keeps is the drag and not a
        // flick: the coast after a release is a *different* promise, measured elsewhere
        for _ in 0..8 {
            self.draw(vec![egui::Event::PointerMoved(at + by)], view, pins);
        }
        self.settle(view, pins);
        let mut up = vec![egui::Event::PointerMoved(at + by)];
        up.push(button(at + by, false));
        self.draw(up, view, pins);
        self.settle(view, pins);
    }

    /// Wheel notches at `at` until the plate is at least `target` times the fit.
    ///
    /// The sign is discovered rather than assumed: a notch that does not zoom in (the plate has a
    /// floor at 1) is sent the other way, and a plate that answers neither fails here.
    fn zoom_to(&mut self, at: Pos2, target: f32, view: &mut MapView, pins: &[Pin]) {
        let mut delta = 120.0;
        for notch in 0..60 {
            if view.camera().zoom() >= target {
                return;
            }
            let mut events = vec![egui::Event::PointerMoved(at)];
            events.push(wheel(delta));
            self.draw(events, view, pins);
            self.settle(view, pins);
            if notch == 0 && view.camera().zoom() <= 1.0 + 1e-4 {
                delta = -delta;
            }
        }
        panic!(
            "60 notches did not take the plate to ×{target}: it is at ×{},
             so neither wheel direction zooms in",
            view.camera().zoom()
        );
    }
}

fn button(at: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn wheel(delta_y: f32) -> egui::Event {
    egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0.0, delta_y),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    }
}

/// Where a place is painted on the plate: the middle of the box, plus the camera's projection —
/// the same arithmetic the view paints through, read from the camera the view publishes.
fn pixel_of(plate: Rect, camera: &Camera, at: Geo) -> Pos2 {
    let point = camera.project(at);
    Pos2::new(plate.center().x + point.x, plate.center().y + point.y)
}

/// A pixel on `iso`'s land, inside the plate and at least [`CLEAR_OF_DOTS`] from every dot.
///
/// A press on a dot is a press on a pin, which is why the land has to be found *clear* of one:
/// the claim here is about the land, and only a press that could not have been a dot can make
/// it. Answers the place and the pixel, so a failure can name both.
fn land_pixel(
    countries: &[Country],
    world: &World,
    iso: &str,
    camera: &Camera,
    plate: Rect,
    pins: &[Pin],
) -> (Geo, Pos2) {
    let dots: Vec<Pos2> = pins
        .iter()
        .map(|pin| pixel_of(plate, camera, Geo::new(pin.lon as f32, pin.lat as f32)))
        .collect();
    let country = countries
        .iter()
        .find(|country| country.iso == iso)
        .unwrap_or_else(|| panic!("the vendored file does not draw {iso}"));
    let mut rings: Vec<&Piece> = country.rings.iter().collect();
    rings.sort_by(|left, right| right.area().total_cmp(&left.area()));
    for ring in rings {
        const STEPS: usize = 24;
        for row in 0..=STEPS {
            let lat = ring.south + (ring.north - ring.south) * (row as f32 / STEPS as f32);
            for column in 0..=STEPS {
                let lon = ring.west + (ring.east - ring.west) * (column as f32 / STEPS as f32);
                let at = Geo::new(lon, lat);
                if world.country_at(at) != Some(iso) {
                    continue;
                }
                let pixel = pixel_of(plate, camera, at);
                if !plate.shrink(2.0).contains(pixel) {
                    continue;
                }
                if dots.iter().any(|dot| dot.distance(pixel) < CLEAR_OF_DOTS) {
                    continue;
                }
                return (at, pixel);
            }
        }
    }
    panic!(
        "no pixel of {iso} is on the plate at ×{} and {CLEAR_OF_DOTS} px clear of all {} dots",
        camera.zoom(),
        dots.len()
    );
}

/// The count `Archive::countries` prints for `iso`: what the rollup row says.
fn counted(archive: &Archive, iso: &str) -> i64 {
    archive
        .countries(168, 400)
        .into_iter()
        .find(|(code, _)| code == iso)
        .map(|(_, count)| count)
        .unwrap_or(0)
}

/// Does the archive say this story is about `iso` somewhere other than its first place?
///
/// Read from the table itself rather than from [`Article`], which carries the one country a
/// story is filed under (`articles.iso`) and not the further rows in `article_countries`:
/// the map's whole point is that a story reaches the second, third and fourth country it
/// names. One read-only connection per question, and only three questions per test.
fn named(article: i64, iso: &str) -> bool {
    let path = atlas_desktop::assets::archive();
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("{} could not be opened: {error}", path.display()));
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM article_countries WHERE article_id = ?1 AND iso = ?2)",
        rusqlite::params![article, iso],
        |row| row.get::<_, i64>(0),
    )
    .map(|found| found == 1)
    .unwrap_or(false)
}

/// The country's name in the vendored file, for the sentence a page prints.
fn name_of(countries: &[Country], iso: &str) -> String {
    countries
        .iter()
        .find(|country| country.iso == iso)
        .map(|country| country.name.clone())
        .unwrap_or_default()
}

/// A press on German land opens Germany's stories: the whole path, from the pixel to the list.
///
/// This is the reader's complaint, made into an assertion. The press is a real press in a real
/// frame of `MapView::show`; what comes back is a [`Pick::Country`] whose code is the one
/// `Archive::country_stories` reads; the rollup [`Door`](atlas_desktop::views::overview::Door)
/// it opens carries the count `Archive::countries` prints; and `app::stories_for` — the read the
/// window makes for that door — returns exactly that list.
#[test]
fn a_press_on_german_land_opens_germanys_story_list() {
    let archive = archive();
    let pins = archive.pins(168, 40);
    let countries = countries();
    let world = world();
    let mut view = MapView::new(&atlas_desktop::assets::geojson());
    let mut page = Page::new(PAGE.0, PAGE.1);
    let plate = page.settle(&mut view, &pins);
    let (at, pixel) = land_pixel(&countries, &world, "DEU", &view.camera(), plate, &pins);

    let answered = page.click(pixel, &mut view, &pins);
    assert_eq!(
        view.picked(),
        Some(&Pick::Country("DEU".to_string())),
        "a press at ({:.1}, {:.1}) px on ({:.2}, {:.2}) landed on {:?}, and that pixel is German \
         land",
        pixel.x,
        pixel.y,
        at.lon,
        at.lat,
        view.picked()
    );
    assert_eq!(
        answered, None,
        "a country is not a pin: `show` answered a pin index for a press on land"
    );

    // the window's own road from the pick to the list
    let Pick::Country(iso) = view.picked().cloned().expect("just asserted") else {
        unreachable!("asserted a country above")
    };
    let count = counted(&archive, &iso);
    let mut choice = Choice::default();
    choice.at_country(&iso, count);
    let door = choice.door().expect("a country door speaks");
    assert_eq!(door.kind, Kind::Country, "a country chip opens a country read: {door:?}");
    assert_eq!(door.word, iso);
    let list: Vec<Article> = app::stories_for(&archive, &door, 168, app::LIST_ROWS);
    assert_eq!(
        list.len() as i64,
        count.min(app::LIST_ROWS),
        "the door {door:?} opens {} rows and its chip counts {count}",
        list.len()
    );
    assert_eq!(
        archive.country_stories(&iso, 168, app::LIST_ROWS).len(),
        list.len(),
        "the door's read and the country's read disagree"
    );
    // A country's list is every story that *names* it, so a row may be filed elsewhere: the
    // archive's `article_countries` holds the other places (1 227 rows, 1.79 a story). What
    // must hold is that each row is one of those — filed under the country or carrying a row
    // for it — and that the country's own list is not silently the primary-only list it used
    // to be (`DEU` read 0 there against a rollup of 3).
    for story in &list {
        assert!(
            story.country == iso || named(story.id, &iso),
            "a story that neither is filed under {iso} nor names it: {:?} ({})",
            story.title,
            story.country
        );
    }
    assert!(count > 0, "Germany has no stories in this window, so the test proves nothing");
    println!(
        "press ({:.1}, {:.1}) px → ({:.2}, {:.2}) → DEU {}, {} stories counted, {} rows opened, \
         newest: {:?}",
        pixel.x,
        pixel.y,
        at.lon,
        at.lat,
        name_of(&countries, &iso),
        count,
        list.len(),
        list.first().map(|story| story.title.as_str()).unwrap_or("")
    );
}

/// A press on land the archive has nothing from still opens a door that names it: `ABW`, a count
/// of 0 and an empty list — a page that can say "Aruba has no stories" instead of nothing.
#[test]
fn a_press_on_land_with_no_stories_still_names_the_country() {
    let archive = archive();
    let pins = archive.pins(168, 40);
    let countries = countries();
    let world = world();
    let filed: Vec<String> = archive
        .countries(168, 400)
        .into_iter()
        .map(|(iso, _)| iso)
        .collect();
    // the first country the file draws that the archive has never filed a story under
    let quiet = countries
        .iter()
        .find(|country| !filed.contains(&country.iso))
        .map(|country| country.iso.clone())
        .expect("every country the file draws has stories");
    let mut view = MapView::new(&atlas_desktop::assets::geojson());
    let mut page = Page::new(PAGE.0, PAGE.1);
    let plate = page.settle(&mut view, &pins);
    let (at, pixel) = land_pixel(&countries, &world, &quiet, &view.camera(), plate, &pins);

    page.click(pixel, &mut view, &pins);
    assert_eq!(
        view.picked(),
        Some(&Pick::Country(quiet.clone())),
        "a press on {quiet}'s land answered {:?}",
        view.picked()
    );
    let count = counted(&archive, &quiet);
    assert_eq!(count, 0, "{quiet} was chosen because the archive has nothing from it");
    let mut choice = Choice::default();
    choice.at_country(&quiet, count);
    let door = choice.door().expect("a country door speaks even with no stories");
    assert!(!door.word.is_empty(), "the door carries no word, so no page can name it");
    let list = app::stories_for(&archive, &door, 168, app::LIST_ROWS);
    assert!(list.is_empty(), "{quiet} listed {} stories", list.len());
    let name = name_of(&countries, &quiet);
    assert!(!name.is_empty(), "the file gives {quiet} no name to print");
    println!(
        "press ({:.1}, {:.1}) px → ({:.2}, {:.2}) → {quiet} {name}: {count} stories, {} rows, \
         the sentence a page needs is \"{} has no stories in the last 72 h.\"",
        pixel.x,
        pixel.y,
        at.lon,
        at.lat,
        list.len(),
        name
    );
}

/// A press on the ocean opens nothing and still pans: the plate moves by exactly the pointer's
/// delta, and neither the press nor the release reports a country.
#[test]
fn a_press_on_the_ocean_pans_and_opens_nothing() {
    let archive = archive();
    let pins = archive.pins(168, 40);
    let world = world();
    let mut view = MapView::new(&atlas_desktop::assets::geojson());
    let mut page = Page::new(1100.0, 760.0);
    let plate = page.settle(&mut view, &pins);
    // the whole world in the box has no slack to pan into, so the plate is zoomed in first, the
    // way a reader would
    page.zoom_to(plate.center(), 6.0, &mut view, &pins);
    let plate = page.settle(&mut view, &pins);
    assert!(
        view.camera().zoom() >= 6.0,
        "the plate is at ×{} and a drag on a plate flush with the box cannot move",
        view.camera().zoom()
    );

    // open water at the middle of the plate: the anchor of the zoom is still the same place
    let middle = view.camera().unproject(Point::new(0.0, 0.0));
    assert_eq!(
        world.country_at(middle),
        None,
        "the middle of the plate is ({:.2}, {:.2}) and that is land",
        middle.lon,
        middle.lat
    );
    let dots: Vec<Pos2> = pins
        .iter()
        .map(|pin| pixel_of(plate, &view.camera(), Geo::new(pin.lon as f32, pin.lat as f32)))
        .collect();
    assert!(
        dots.iter().all(|dot| dot.distance(plate.center()) >= CLEAR_OF_DOTS),
        "a dot sits on the middle of the plate, so a press there is a press on a pin"
    );

    let before = view.camera().offset();
    let by = Vec2::new(60.0, -40.0);
    page.drag(plate.center(), by, &mut view, &pins);
    let after = view.camera().offset();
    let moved = Vec2::new(after.x - before.x, after.y - before.y);
    assert!(
        (moved - by).length() <= 1.0,
        "the pointer moved {by:?} and the plate moved {moved:?} (×{})",
        view.camera().zoom()
    );
    assert_eq!(view.picked(), None, "a drag on open water opened {:?}", view.picked());

    // and a press with no drag: nothing opens, and the plate does not jump
    let held = view.camera().offset();
    let answered = page.click(plate.center(), &mut view, &pins);
    assert_eq!(answered, None, "a press on open water answered a pin index");
    assert_eq!(view.picked(), None, "a press on open water opened {:?}", view.picked());
    let parked = view.camera().offset();
    assert!(
        (parked.x - held.x).abs() <= 0.01 && (parked.y - held.y).abs() <= 0.01,
        "a press on open water moved the plate from {held:?} to {parked:?}"
    );
    println!(
        "at ×{:.1}, the centre is ({:.2}, {:.2}) — open water; a {by:?} drag moved the plate \
         {moved:?}, and neither the drag nor the press opened anything",
        view.camera().zoom(),
        middle.lon,
        middle.lat
    );
}
