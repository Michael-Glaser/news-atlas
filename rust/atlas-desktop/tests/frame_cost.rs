//! What a frame of the native window costs, driven through the real window.
//!
//! `tests/perf.rs` measures the *statements*: what `Archive::counts`, `pins`, `articles`
//! and the rest cost in isolation. That is half the story, and the wrong half to stop at:
//! a page can be cheap at the database and expensive to paint (a 1 632-coastline map
//! re-projected, loaded and filled every frame), and a page can be expensive at the
//! database and cheap to paint (a list whose rows are already in memory). This file
//! measures the frame — `run_ui` plus `tessellate`, the two things a window does between
//! two mouse events — and the one thing that would make every number in here irrelevant:
//! a frame that reads the archive.
//!
//! # What is measured
//!
//! * **The first Overview frame** — measured 15.7–23.7 ms in release across runs on this
//!   machine, of which 10.7–18.4 ms is `views::map::World::load` parsing 3.0 MB of GeoJSON
//!   (51–78% of the frame, and the plate's own note says the same). The map is built lazily (`views/overview.rs`,
//!   `get_or_insert_with(MapView::new)`), so the parse lands in frame 1 and not in
//!   `Atlas::new`. The budget is a ceiling on the frame *and* an assertion that the parse
//!   is inside it: a change that moves the parse off the first frame should make the second
//!   assertion fail and be re-stated on purpose.
//! * **A steady Overview frame** — 1.02–1.32 ms release: the rail, the status row, the
//!   plate with its fill mesh, and 252 story rows.
//! * **An Articles frame** (list and reading pane, 0.38–0.40 ms) and **a Graph frame** (the
//!   settling graph and its panel, 0.13–0.29 ms).
//! * **Frame 17 against frame 5** — the same drawing twice, twelve paints apart. A frame
//!   that grows with the number of paints is the per-frame clone `app.rs` was rewritten to
//!   remove, and no statement-level budget can see it.
//! * **A zoom sweep** — the plate pushed from the world view to ×93 and back in forty
//!   steps, with the *worst* step of the forty asserted and printed, not the mean. The
//!   complaint this answers is "it stutters when I zoom in", and the measurement has a
//!   result worth stating: the expensive end is the **world view** (1.0–1.8 ms release,
//!   15 491 segments of coast and a fill mesh over the land), and ×96 is free — steps 12 to
//!   30 are below this clock's resolution, 0.0 ms, because one country's fill and a few
//!   hundred segments are all that is on screen. An average of the forty would have
//!   reported 0.19–0.3 ms and hidden both.
//! * **A click on a pin** — one frame (1.12–1.68 ms release), the pin returned, its label
//!   still on the plate with the pointer taken away, and the window's own read counter
//!   unchanged: selecting a place is not a reason to read the archive again.
//! * **A keystroke** — one read of the archive, not one read per frame, counted two ways:
//!   the window's own counter (the `reads` field behind the status row, read off the painted
//!   text because the field is private) across frames and a *typed* search, and the eight
//!   statements a read is, timed directly (2.3–2.6 ms release).
//!
//! # The profile
//!
//! The numbers quoted on each budget are **release** numbers. These tests run in whatever
//! profile `cargo test` picks, which is debug, and a debug build parses JSON and shapes
//! text many times slower; a `#![cfg(not(debug_assertions))]` file would test nothing on
//! the machine that runs `cargo test`. So every budget is stated in release terms and
//! multiplied by [`PROFILE`] when `debug_assertions` is on, and the multiplier is the
//! ratio measured between the two profiles here.
//!
//! Run it, with the numbers printed:
//!
//! ```text
//! cargo test --offline -p atlas-desktop --test frame_cost -- --nocapture
//! cargo test --release --offline -p atlas-desktop --test frame_cost -- --nocapture
//! ```
//!
//! Read-only, no window, no socket: `data::Archive` opens the archive with
//! `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only`, nothing here writes, spawns or
//! listens, and `egui::Context` is driven with a screen rectangle, a clock and synthetic
//! pointer events — no GPU surface is ever asked for.

use std::path::Path;
use std::time::{Duration, Instant};

use egui::epaint::Shape;
use egui::{Pos2, Rect, Vec2};

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::assets;
use atlas_desktop::data::{Archive, Article, Pin};
use atlas_desktop::theme;

use atlas_desktop::views::map::MapView;

/// The window the app opens at, near enough: `main.rs` asks for 1 280 × 860.
const SCREEN: Vec2 = Vec2::new(1280.0, 860.0);
/// The window `app::Atlas::new` opens on, and the limits `reload` asks each list for.
const HOURS: i64 = 72;
const PINS: i64 = 400;
const COUNTRIES: i64 = 14;
const ARTICLES: i64 = 400;
const ENTITIES: i64 = 120;
const EDGES: i64 = 900;
/// The rail's width: anything painted to the right of it belongs to the page.
const RAIL: f32 = 188.0;

/// How much slower a debug build is than a release build on this machine, per
/// measurement in this file. Measured, not guessed, comparing the two profiles on the
/// same archive in the same session: a Graph frame is 2.3 ms debug against 0.13 ms
/// release (×18), a steady Overview frame 17.5 against 1.09 (×16), an Articles frame 5.4
/// against 0.38 (×14), the first frame 172 against 15.7 (×11), the GeoJSON parse 90
/// against 10.7 (×8), one read with a needle 4.5 against 2.3 (×2). Twenty-five is above
/// the largest of those with room for a slower machine, and a budget fifty times the
/// measured cost still catches the failure it exists for in either profile: a parse per
/// frame is ×10 000, a read per frame is milliseconds on a microsecond page.
///
/// In a debug build the budgets are therefore a smoke alarm for an order-of-magnitude
/// regression, not a performance target. The numbers that ship are the release ones, and
/// the *structural* assertions — the parse inside the first frame, the same cost after
/// twelve paints, one read per keystroke, a click that reads nothing — hold in both.
const DEBUG_FACTOR: f64 = 25.0;

/// The multiplier a release-stated budget is read with in this build.
const PROFILE: f64 = if cfg!(debug_assertions) { DEBUG_FACTOR } else { 1.0 };

// --------------------------------------------------------------------- budgets

/// The first Overview frame, release: measured min 15.7–23.7 ms / median 16.6–25.0 ms
/// across runs, and 34.0 ms with other agents building, of which the parse is 10.7–18.4 ms
/// (51–78%). Four times the slower minimum: this is the slowest frame in the app and the one
/// a reader waits for, and the margin is for a colder page cache, not for a second parse.
const FIRST_FRAME_MS: f64 = 100.0;
/// A steady Overview frame — the rail, the status row, the plate with its fill mesh, and
/// 252 story rows — release: measured min 1.02–1.32 ms / median 1.6–1.8 ms for `run_ui` plus
/// `tessellate`. The ceiling is about three times that and about the cost of one archive
/// read (2.3–4.0 ms measured below), so a frame that started reading cannot hide inside it.
const STEADY_FRAME_MS: f64 = 4.0;
/// An Articles frame with a story open in the reading pane, release: measured min
/// 0.38–0.40 ms / median 0.53–0.73 ms for 252 list rows. Six times the minimum, still an
/// order of magnitude under one read.
const ARTICLES_FRAME_MS: f64 = 2.5;
/// A Graph frame — 120 nodes, 232 relations, one `tick`, then the paint — release:
/// measured min 0.13–0.29 ms / median 0.15–0.31 ms. Five times the slower minimum: the graph
/// is the cheapest page in the app and the budget exists to keep it there when the window
/// grows.
const GRAPH_FRAME_MS: f64 = 1.5;
/// The **worst step** of a forty-step zoom sweep from the world view to ×93 and back,
/// release: measured 1.0–1.8 ms, and it is step 2 — the world view, ×1.1 — not ×96, where
/// steps 12–30 are below the clock. Six times the slower measurement: the failure this
/// catches is the fill mesh going uncapped, which the map's own `FILL_COST` note measures at
/// 2.6 ms for the mesh alone.
const ZOOM_WORST_FRAME_MS: f64 = 12.0;
/// A frame in which a click lands on a pin, release: measured 1.12–1.68 ms (best of three
/// clicks) — the hit test is over the drawn dots, not over the archive, so it costs what any
/// other steady Overview frame costs. Three times the slower measurement.
const PIN_FRAME_MS: f64 = 5.0;
/// One keystroke: a read of the window's articles with a needle over the bodies of the
/// window. The whole read set is timed here — the eight statements `reload` runs, of which
/// `edges` is the slow one — release, measured min 2.3–4.0 ms / median 2.5–4.6 ms. Three
/// times the slower measurement. The failure this catches is a `LIKE` that loses its index: measured on a
/// window twelve times bigger, the search alone was 100 ms.
const KEYSTROKE_MS: f64 = 12.0;
/// How much a frame may cost after seventeen paints compared with the fifth. Release,
/// measured 0.90–1.03× in three runs; debug 1.03–1.35×. Two is the ceiling because a *real*
/// accumulation — the per-frame `Snapshot` clone this crate was rewritten to remove — is not
/// 2×, it is the whole read repeated per paint (×100).
const GROWTH: f64 = 2.0;
/// How far the sweep must actually zoom for its measurement to mean anything: the
/// camera eases toward the scroll's target one frame at a time, so a sweep that never
/// arrives would measure forty world views and call it a zoom.
const SWEEP_TARGET_ZOOM: f32 = 90.0;

// ------------------------------------------------------------------- the read

/// What one read of the archive answered that a page shows: the plate's pins and the
/// list's rows. The counts, topics, monitors and the graph's half are read as well (they
/// are part of a read's cost) and dropped, because no test in this file draws them.
struct Loaded {
    pins: Vec<Pin>,
    articles: Vec<Article>,
}

/// One read of the archive: `app::Atlas::reload`'s eight statements, in its order and
/// with its limits (`src/app.rs`, `fn reload`).
///
/// `calls` is the counter the keystroke test asserts on: it is bumped once per statement
/// actually run, so "one read per keystroke" is counted rather than claimed. The drawing
/// path takes a `&Snapshot` and has no `Archive` in scope, so a frame cannot bump it —
/// which is the property being measured, not an assumption made here. (The window's own
/// counter, `Atlas::reads`, is asserted separately, from the status row it paints.)
fn read(archive: &Archive, hours: i64, needle: &str, calls: &mut usize) -> Loaded {
    // Counted after each statement, not before: a statement that panics did not read.
    let pins = archive.pins(hours, PINS);
    *calls += 1;
    let _countries = archive.countries(hours, COUNTRIES);
    *calls += 1;
    let articles = archive.articles(hours, ARTICLES, needle);
    *calls += 1;
    let _counts = archive.counts();
    *calls += 1;
    // The rail's lists: read with the snapshot, shown on two pages of their own.
    let _topics = archive.topics();
    *calls += 1;
    let _monitors = archive.monitors();
    *calls += 1;
    let entities = archive.entities(hours, ENTITIES);
    *calls += 1;
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let _edges = archive.edges(&ids, EDGES);
    *calls += 1;
    Loaded { pins, articles }
}

// ------------------------------------------------------------------ the frame

/// A context with the app's own style installed, as `Atlas::new` does, so a page is
/// measured with the fonts, spacing and palette it ships with.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// The input of one frame: a screen, no pointer, no keys, a clock that moves, so an
/// animation (the map's easing, the graph's cooling) steps instead of waiting.
fn frame_input(index: usize) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(index as f64 / 60.0),
        ..Default::default()
    }
}

/// One frame: `run_ui` — egui 0.37 hands the callback the root `Ui`, and `Atlas::draw`
/// takes exactly that — then `tessellate`, because a frame is not over until its shapes
/// are triangles. Answers what the frame cost and what reached the screen.
///
/// `epaint 0.37` panics on `Drop` when a `TexturesDelta` still carries unapplied deltas
/// (`epaint-0.37.0/src/textures.rs:337`, "Deltas need to be handled"); a real backend
/// uploads them, and there is no renderer here, so the deltas are dropped explicitly
/// through the crate's own `drop_without_applying_deltas`.
fn frame(ctx: &egui::Context, input: egui::RawInput, mut draw: impl FnMut(&mut egui::Ui)) -> (Duration, Painted) {
    let started = Instant::now();
    let mut output = ctx.run_ui(input, |ui| draw(ui));
    let pixels_per_point = output.pixels_per_point;
    let shapes = std::mem::take(&mut output.shapes);
    output.drop_without_applying_deltas();
    let painted = Painted::measure(&shapes);
    let _primitives = ctx.tessellate(shapes, pixels_per_point);
    (started.elapsed(), painted)
}

/// What one frame painted, as far as a test can see it: every string, where each landed,
/// and every dot.
///
/// The strings are the window's own claims about itself — the status row says how many
/// reads the archive has had, the map's caption says what zoom and how many segments —
/// so a test reads the same numbers a reader does instead of reaching into a field.
#[derive(Default)]
struct Painted {
    text: String,
    labels: Vec<(String, Rect)>,
    /// Every `Shape::Circle` centre and radius, in paint order.
    circles: Vec<(Pos2, f32)>,
    shapes: usize,
}

impl Painted {
    fn measure(shapes: &[egui::epaint::ClippedShape]) -> Painted {
        let mut painted = Painted::default();
        for clipped in shapes {
            painted.walk(&clipped.shape);
        }
        painted
    }

    fn walk(&mut self, shape: &Shape) {
        self.shapes += 1;
        match shape {
            Shape::Vec(items) => {
                for item in items {
                    self.walk(item);
                }
            }
            Shape::Text(text) => {
                let string = text.galley.text().to_string();
                self.labels
                    .push((string.clone(), text.galley.rect.translate(text.pos.to_vec2())));
                self.text.push_str(&string);
                self.text.push('\n');
            }
            Shape::Circle(circle) => self.circles.push((circle.center, circle.radius)),
            _ => {}
        }
    }

    /// Where the first string *containing* `needle` was painted, if any.
    fn rect_containing(&self, needle: &str) -> Option<Rect> {
        self.labels
            .iter()
            .find(|(text, _)| text.contains(needle))
            .map(|(_, rect)| *rect)
    }

    /// The window's own read counter, as the status row prints it: `… · 3 reads`.
    fn reads(&self) -> Option<u32> {
        for line in self.text.lines() {
            for part in line.split(" · ") {
                let part = part.trim();
                if let Some(number) = part.strip_suffix(" reads").or_else(|| part.strip_suffix(" read")) {
                    if let Ok(value) = number.trim().parse::<u32>() {
                        return Some(value);
                    }
                }
            }
        }
        None
    }

    /// The map's caption, `zoom ×12.3 · 33 places · 4 120 segments · …`, as two numbers.
    fn plate(&self) -> Option<(f32, usize)> {
        let line = self.text.lines().find(|line| line.starts_with("zoom ×"))?;
        let zoom = line.strip_prefix("zoom ×")?.split_whitespace().next()?.parse().ok()?;
        let segments = line
            .split(" · ")
            .find_map(|part| part.strip_suffix(" segments"))
            .and_then(|number| number.replace(' ', "").parse().ok())?;
        Some((zoom, segments))
    }

    /// The dots the plate painted: the page's pins, right of the rail.
    ///
    /// The rail is left of `RAIL` and draws the mark with circles of its own; the plate
    /// is the only thing drawing filled circles to the right of it.
    fn dots(&self) -> Vec<(Pos2, f32)> {
        self.circles
            .iter()
            .copied()
            .filter(|(at, _)| at.x > RAIL)
            .collect()
    }
}

// ------------------------------------------------------------------ the table

/// Samples of one measurement, kept: a minimum, a median and a maximum say very
/// different things on a machine other agents are building on.
struct Sampled {
    what: &'static str,
    samples: Vec<Duration>,
}

impl Sampled {
    fn new(what: &'static str) -> Sampled {
        Sampled {
            what,
            samples: Vec::new(),
        }
    }

    fn take(&mut self, spent: Duration) {
        self.samples.push(spent);
    }

    fn ms(spent: Duration) -> f64 {
        spent.as_secs_f64() * 1000.0
    }

    fn min(&self) -> f64 {
        self.samples.iter().copied().min().map(Sampled::ms).unwrap_or(0.0)
    }

    fn median(&self) -> f64 {
        let mut sorted = self.samples.clone();
        if sorted.is_empty() {
            return 0.0;
        }
        sorted.sort();
        Sampled::ms(sorted[sorted.len() / 2])
    }

    fn max(&self) -> f64 {
        self.samples.iter().copied().max().map(Sampled::ms).unwrap_or(0.0)
    }

    fn print(&self, budget_ms: f64) {
        eprintln!(
            "{:<34} min {:>8.3}ms  median {:>8.3}ms  max {:>8.3}ms  ({} samples, budget {:.1}ms{})",
            self.what,
            self.min(),
            self.median(),
            self.max(),
            self.samples.len(),
            budget_ms,
            if cfg!(debug_assertions) { ", debug" } else { "" }
        );
    }

    fn assert_under(&self, budget_ms: f64, why: &str) {
        assert!(
            self.min() <= budget_ms,
            "{}: its fastest run was {:.3} ms, over the {:.1} ms budget ({why}). All samples: {:?}",
            self.what,
            self.min(),
            budget_ms,
            self.samples
                .iter()
                .map(|spent| format!("{:.3}", Sampled::ms(*spent)))
                .collect::<Vec<_>>()
        );
    }
}

// ----------------------------------------------------------------- the pieces

/// The checkout's archive, or a sentence and no measurement: a bare clone has nothing to
/// cost. Same rule as `tests/perf.rs` — skip, never fail.
fn archive_or_skip() -> Option<Archive> {
    let path = assets::archive();
    if !path.is_file() {
        eprintln!(
            "no archive at {} — nothing to measure; start ATLAS once so it is created",
            path.display()
        );
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some(archive),
        Err(sentence) => {
            eprintln!("{sentence} — nothing to measure");
            None
        }
    }
}

/// The map's outlines, or nothing to measure: the first-frame and zoom tests are about
/// the plate.
fn geojson_or_skip() -> Option<std::path::PathBuf> {
    let path = assets::geojson();
    if !path.is_file() {
        eprintln!(
            "no outlines at {} — the plate has nothing to parse or fill; nothing to measure",
            path.display()
        );
        return None;
    }
    Some(path)
}

/// The window the app opens, on the checkout's archive, with one frame drawn: the frame
/// the shipped window draws, because `Atlas::with_archive` is `Atlas::new` minus the
/// `CreationContext` that only a window can build.
fn window_or_skip() -> Option<Atlas> {
    let path = assets::archive();
    if !path.is_file() {
        eprintln!(
            "no archive at {} — nothing to measure; start ATLAS once so it is created",
            path.display()
        );
        return None;
    }
    // No frame is drawn here: the first frame *is* one of the measurements, and a helper
    // that drew one to check itself would hand every later test a warm window (measured:
    // the "first" frame then costs 51.8 ms against a parse of 120 ms, because the parse
    // had already happened in the helper).
    Some(Atlas::with_archive(path))
}

/// A press and a release at `at`, the two frames a reader's click is, each drawn by
/// `draw` so the widget under the point is the one that answers.
fn click(
    ctx: &egui::Context,
    index: usize,
    at: Pos2,
    draw: impl FnMut(&mut egui::Ui),
) -> (Duration, Painted) {
    let mut draw = draw;
    let events = |pressed: bool| egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(index as f64 / 60.0),
        events: vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..Default::default()
    };
    let _press = frame(ctx, events(true), &mut draw);
    // The release is the frame that answers: `Response::clicked()` is true on it.
    frame(ctx, events(false), draw)
}

// --------------------------------------------------------------------- tests

impl Painted {
    /// Whether the plate was painted at all: the map's caption is the plate's own line.
    fn paints_the_plate(&self) -> bool {
        self.text.contains("drag to pan, scroll to zoom")
    }
}

/// The first frame of the window: the plate's 3.0 MB of outlines, parsed, plus the page
/// that first shows them.
///
/// This is the number a reader feels on a cold start, and the one number in this crate
/// that a change of *shape*, not of tuning, once left out: the parse happens inside
/// frame 1 because the Overview builds its map lazily.
#[test]
fn the_first_overview_frame_is_the_geojson_parse() {
    let Some(geojson) = geojson_or_skip() else {
        return;
    };
    if !assets::archive().is_file() {
        eprintln!("no archive — the window has no page to draw; nothing to measure");
        return;
    }

    // The parse on its own, three times over: the same call `MapView::new` makes inside
    // the first frame (`views/map.rs`, `World::load`).
    let mut parse = Sampled::new("map: World::load (3.0 MB GeoJSON)");
    let mut note = String::new();
    for _ in 0..3 {
        let started = Instant::now();
        let view = MapView::new(Path::new(&geojson));
        parse.take(started.elapsed());
        assert!(
            view.outlines().contains("coastlines"),
            "the outlines did not load: {}",
            view.outlines()
        );
        // What the plate itself says the parse cost, read from its own accessor. The card
        // paints the same sentence, but a label in a title row is truncated when the window
        // is narrow — measured: `… 1 632 coastli…`, with the parse time cut off — so the
        // note is taken from the plate's accessor, not from the painted text.
        note = view.outlines();
    }

    // The first frame, from a cold context and a fresh window — three times, because one
    // cold frame is one sample of a machine other agents are building on.
    let mut first = Sampled::new("frame 1: Overview (map parse inside)");
    for sample in 0..3 {
        let ctx = context();
        let Some(mut atlas) = window_or_skip() else {
            return;
        };
        let (spent, painted) = frame(&ctx, frame_input(sample), |ui| atlas.draw(ui));
        assert!(
            painted.paints_the_plate(),
            "the first frame drew no plate: the outlines were not parsed inside it"
        );
        assert!(
            painted.reads().unwrap_or(0) >= 1,
            "the first frame painted no read count — the status row changed shape, so the \
             window below it is not the one this file measures"
        );
        first.take(spent);
    }

    eprintln!("  the plate's own note: {note}");
    parse.print(FIRST_FRAME_MS);
    first.print(FIRST_FRAME_MS);

    // The parse is inside the frame, so the frame cannot be *much* cheaper than the parse.
    // "Much" is the 15 % a machine under load moves a 1 s measurement by: the two numbers are
    // the minimum of three samples each, taken minutes apart, and the parse loop's first sample
    // reads 3 MB of GeoJSON from a cold page cache while the frame's does not. Measured before
    // this bound existed: parse 1053.6 ms against a first frame of 978.7 ms — 7 % apart, with
    // the plate's own note (`1 632 coastlines · 99 613 points · outlines read in 1054 ms`) in
    // the same frame, i.e. the parse was inside it. A change that really moves the parse out of
    // the frame (a worker, a cache) lands far past 15 % and still fails here.
    const NOISE: f64 = 0.15;
    assert!(
        first.min() * (1.0 + NOISE) >= parse.min(),
        "the first frame took {:.1} ms but the parse alone {:.1} ms — two numbers {:.0}% apart, \
         more than this machine's noise: the parse is not inside the frame any more (a worker?), \
         and the share below would measure nothing",
        first.min(),
        parse.min(),
        (parse.min() / first.min().max(0.001) - 1.0) * 100.0
    );
    let share = (parse.min() / first.min().max(0.001)).min(1.0);
    eprintln!(
        "  the parse is {:.0}% of the first frame ({:.1} ms of {:.1} ms)",
        share * 100.0,
        parse.min(),
        first.min()
    );
    // The claim the document makes: the parse is the dominant part of the first frame.
    //
    // In release it is: 60–80% of the frame. In a debug build it is 22%, because shaping
    // 252 story rows costs more there than parsing 3 MB of JSON does — measured, debug
    // first frame 622 ms against a 136 ms parse, release 21 ms against 13 ms. So the
    // release floor is the claim; the debug floor only has to show that the parse is a
    // real part of the frame and not a rounding error.
    let floor = if cfg!(debug_assertions) { 0.15 } else { 0.4 };
    assert!(
        share >= floor,
        "the GeoJSON parse is only {:.0}% of the first frame — it was measured at 60–80% \
         in release and 22% in debug (where text shaping is the larger half), and it is \
         the reason the first frame costs what it does; a change that moved work out of \
         the parse should restate this budget, not fail it silently",
        share * 100.0
    );
    first.assert_under(
        FIRST_FRAME_MS * PROFILE,
        "a first frame that parses the outlines once",
    );
}

/// The Overview, warm: no parse, no query, just the page drawn from memory.
#[test]
fn a_steady_overview_frame_costs_what_it_draws() {
    if !assets::archive().is_file() || !assets::geojson().is_file() {
        eprintln!("no archive or no outlines — nothing to measure");
        return;
    }
    let ctx = context();
    let Some(mut atlas) = window_or_skip() else {
        return;
    };

    // Frames 1–3 pay the parse and the font atlas; the ones after are what a program
    // reads in.
    for index in 1..4 {
        frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
    }
    // Twenty-four paints, not eight. Measured on this machine against the same build, the
    // cheapest of eight samples moved between 0.58 ms and 2.7 ms as other agents compiled
    // (load average 4–64): a scheduler slice lands on some samples and not on others. A
    // stolen slice can only make a frame look *slower*, so more samples only make the
    // minimum truer — a page that reads or parses per paint is slow in every sample, and the
    // budget below is unchanged.
    let mut steady = Sampled::new("frame: Overview, steady");
    for index in 4..28 {
        let (spent, painted) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        assert!(
            painted.paints_the_plate(),
            "the plate vanished from a steady frame"
        );
        steady.take(spent);
    }
    // The same seconds, the same context: one label, for the yardstick.
    let mut overhead = Sampled::new("frame: control (one label)");
    for index in 28..36 {
        let (spent, _) = frame(&ctx, frame_input(index), |ui| {
            ui.label("control");
        });
        overhead.take(spent);
    }
    // A cold first frame, for the comparison the page's laziness rests on.
    let mut first = Sampled::new("frame 1: Overview, for the ratio");
    {
        let cold = context();
        let Some(mut fresh) = window_or_skip() else {
            return;
        };
        let (spent, _) = frame(&cold, frame_input(0), |ui| fresh.draw(ui));
        first.take(spent);
    }

    steady.print(STEADY_FRAME_MS);
    overhead.print(STEADY_FRAME_MS);
    first.print(FIRST_FRAME_MS);
    let ratio = first.min() / steady.min().max(0.001);
    let pages = steady.min() / overhead.min().max(0.001);
    eprintln!("  the first frame costs {ratio:.0}× a steady one");
    eprintln!(
        "  a steady frame costs {pages:.1}× an empty one ({:.3} ms)",
        overhead.min()
    );
    // The parse happens once: a steady frame may not cost anything like the first one.
    // Measured ×5 in debug and ×9 in release; 2.5 is the floor, because the plate's own
    // paint is a large share of a steady frame and a page that re-parsed per frame would
    // be ×20 or worse, not ×2.5.
    assert!(
        ratio >= 2.5,
        "a steady frame costs {:.2} ms against a first frame of {:.1} ms (×{:.1}) — the \
         outlines are being parsed per frame, or something else in the page reads the \
         archive or the disk every paint",
        steady.min(),
        first.min(),
        ratio
    );
    steady.assert_under(
        STEADY_FRAME_MS * PROFILE,
        "a page drawn from memory: no query, no parse, no copy",
    );
}

/// The Articles page with a story open — the list and the reading pane — and the Graph
/// page with the window's graph settling.
#[test]
fn an_articles_frame_and_a_graph_frame_cost_what_they_draw() {
    if !assets::archive().is_file() {
        eprintln!("no archive — nothing to measure");
        return;
    }
    let ctx = context();
    let Some(mut atlas) = window_or_skip() else {
        return;
    };
    atlas.set_page(Page::Articles);
    for index in 1..3 {
        frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
    }
    let mut articles_frame = Sampled::new("frame: Articles (list + reading pane)");
    // Sixteen paints: the same reason as the Overview's twenty-four — a slice stolen from
    // one sample inflates it, the minimum is what the budget reads, and more samples make
    // the minimum truer without moving the budget.
    for index in 3..19 {
        let (spent, painted) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        assert!(
            painted.text.contains("stories in the last"),
            "the Articles page did not draw its list"
        );
        articles_frame.take(spent);
    }

    atlas.set_page(Page::Graph);
    for index in 19..21 {
        frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
    }
    let mut graph_frame = Sampled::new("frame: Graph (nodes, settling)");
    for index in 21..37 {
        let (spent, painted) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        assert!(
            painted.text.contains("in the window — click a node"),
            "the Graph page did not draw"
        );
        graph_frame.take(spent);
    }

    articles_frame.print(ARTICLES_FRAME_MS);
    graph_frame.print(GRAPH_FRAME_MS);
    articles_frame.assert_under(
        ARTICLES_FRAME_MS * PROFILE,
        "a list of rows already in memory plus one story's text",
    );
    graph_frame.assert_under(
        GRAPH_FRAME_MS * PROFILE,
        "one alpha tick over the window's nodes and relations, then the paint",
    );
}

/// The same drawing, twenty-four frames apart. A frame that grows with the number of paints is
/// the per-frame clone `app.rs` was rewritten to remove — no statement budget sees it, and
/// no single-frame measurement does either.
#[test]
fn a_frame_costs_the_same_on_the_second_paint_as_on_the_tenth() {
    if !assets::archive().is_file() || !assets::geojson().is_file() {
        eprintln!("no archive or no outlines — nothing to measure");
        return;
    }
    let ctx = context();
    let Some(mut atlas) = window_or_skip() else {
        return;
    };

    // Frames 1–4 are not the subject: frame 1 parses the outlines, and the next ones pay
    // the font atlas and the galley cache being built (measured here in debug: frame 2
    // costs four times frame 8 on the same page, and the page does nothing new). A
    // *growth* budget that started at frame 2 would measure the warm-up, not growth.
    for index in 0..4 {
        frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
    }
    let mut early = Sampled::new("frame 5–16: Overview");
    for index in 4..16 {
        let (spent, _) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        early.take(spent);
    }
    // The same drawing, twenty-four paints later. Twelve samples a window rather than six:
    // measured on this machine at load average 43.8, the six-sample late window caught a
    // 76.7 ms scheduler stall in its minimum (3.923 ms) and reported a growth of 2.49 on a
    // page that is flat when the machine is quiet (0.47–1.13×). A stall can only add time,
    // so a wider window makes the minimum truer; the ceiling is unchanged, and a page that
    // accumulates is slow in every one of the twelve.
    let mut late = Sampled::new("frame 29–40: Overview");
    for index in 28..40 {
        let (spent, _) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        late.take(spent);
    }

    early.print(STEADY_FRAME_MS);
    late.print(STEADY_FRAME_MS);
    let growth = late.min() / early.min().max(0.001);
    eprintln!("  frame 29–40 costs {growth:.2}× frame 5–16");
    assert!(
        growth <= GROWTH,
        "the same page costs {:.2} ms after twenty-nine paints and {:.2} ms after five \
         (×{:.2}) — something in the frame accumulates (a clone, a growing list, a \
         texture per paint)",
        late.min(),
        early.min(),
        growth
    );
    late.assert_under(
        STEADY_FRAME_MS * PROFILE,
        "the twenty-ninth paint of a page that was already drawn",
    );
}

/// The plate's own worst case: the world view, ×93, and back, in forty steps of a scroll
/// wheel — the worst of the forty frames asserted, never the mean.
///
/// The complaint this answers is "zooming stutters". The measurement says where it does
/// not, and where it would: the expensive end of a sweep is the **world view**, where the
/// fill mesh covers whole continents and 15 491 segments of coastline are on screen
/// (1.0–1.1 ms release, 19–24 ms debug), and ×96 is free — steps 12 to 30 are below this
/// clock's resolution (0.0 ms), because one country's fill and a few hundred segments are
/// all that is left. The worst step of the forty is step 2, the world view itself, at
/// 1.0–1.3 ms release and 15–24 ms debug. A mean of the forty would have reported 0.19 ms
/// release and hidden all of it, which is why the worst step is what is printed and
/// asserted.
///
/// The plate is drawn here in a panel whose margin is the app's, at a height of 470, so
/// the pointer's coordinates are the plate's own: `MapView::show` takes the whole
/// available width, and the hover point is the middle of that box. No remembered screen
/// coordinates, and no `Camera` (it is not public since the map adopted `geo`): the sweep
/// goes through the same wheel event a reader's mouse sends.
#[test]
fn a_zoom_sweep_to_96x_has_no_frame_over_its_budget() {
    let Some(geojson) = geojson_or_skip() else {
        return;
    };
    let Some(archive) = archive_or_skip() else {
        return;
    };
    let mut calls = 0;
    let pins = read(&archive, HOURS, "", &mut calls).pins;
    assert!(
        !pins.is_empty(),
        "the window holds no pin, so the plate drew no dot to look at"
    );

    let ctx = context();
    let margin = 20.0;
    let height = 470.0;
    let hover = Pos2::new(SCREEN.x / 2.0, margin + height / 2.0);

    const STEPS: usize = 40;
    // Enough wheel per step to ask for more than ×96 over twenty of them, because the
    // camera eases toward the target the scroll sets instead of arriving in one frame; the
    // twenty back steps ask for the same in reverse. Forty frames of motion is what a
    // reader's flick is.
    const SCROLL: f32 = 170.0;
    // Passes over the whole sweep. A step's cost is its own only at its floor: another
    // process on this machine can add milliseconds to one frame and never take them away
    // (`tests/perf.rs` says the same about a statement), so each step keeps its best pass
    // and the *worst step* below is the worst of forty such floors. Measured on a machine
    // with other agents building: without the passes, one loaded frame reported 19.8 ms for
    // a step that costs 0.5 ms.
    const PASSES: usize = 3;

    let mut floors = vec![f64::INFINITY; STEPS];
    let mut zooms = vec![f32::NAN; STEPS];
    let (mut zoom_at_fit, mut segments_at_fit) = (f32::NAN, 0);
    let (mut zoom_back, mut segments_back, mut peak) = (f32::NAN, 0, 0.0_f32);

    for pass in 0..PASSES {
        // A fresh plate each pass, so the camera starts from the fit every time and the
        // forty steps are the same forty steps.
        let mut view = MapView::new(Path::new(&geojson));
        let mut draw = |index: usize, scroll: f32| {
            // The clock keeps moving across passes: egui refuses a frame whose time is
            // earlier than the last one's ("Time shouldn't move backwards"), and a pass
            // that restarted it would fail for a reason that has nothing to do with the
            // plate.
            let mut input = frame_input(pass * 100 + index);
            input.events.push(egui::Event::PointerMoved(hover));
            if scroll != 0.0 {
                // What a wheel sends: egui turns it into the `smooth_scroll_delta` the plate
                // reads (`views/map.rs`), so the sweep drives the real gesture path.
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, scroll),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::default(),
                });
            }
            frame(&ctx, input, |ui| {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(margin as i8, 16)),
                    )
                    .show(ui, |ui| {
                        let _ = view.show(ui, &pins, height);
                    });
            })
        };

        // Frame zero seats the camera (`MapView::show` sets it from the box on its first
        // paint), and the frames after it pay the font atlas and the galley cache being
        // built — measured here, the first four frames of an unchanged plate cost more than
        // twice the tenth. Neither is part of the sweep: a "worst frame" that is really the
        // first frame ever drawn would measure egui's warm-up, not the zoom.
        let (_, fitted) = draw(0, 0.0);
        let (fit_zoom, fit_segments) = fitted.plate().expect("the plate painted no caption");
        if pass == 0 {
            zoom_at_fit = fit_zoom;
            segments_at_fit = fit_segments;
        }
        for warm in 1..7 {
            draw(warm, 0.0);
        }
        for step in 0..STEPS {
            let scroll = if step < STEPS / 2 { SCROLL } else { -SCROLL };
            let (spent, painted) = draw(7 + step, scroll);
            floors[step] = floors[step].min(Sampled::ms(spent));
            if let Some((zoom, _)) = painted.plate() {
                zooms[step] = zoom;
                peak = peak.max(zoom);
            }
        }
        // The frames after the last scroll: the camera finishes the ease it was asked for
        // (a flick's momentum, with no wheel behind it).
        for settle in 0..24 {
            let (_, settled) = draw(47 + settle, 0.0);
            if let Some((zoom, segments)) = settled.plate() {
                zoom_back = zoom;
                segments_back = segments;
            }
        }
    }

    let (worst_step, worst_ms) = floors
        .iter()
        .copied()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(step, ms)| (step + 1, ms))
        .expect("forty steps were swept");
    let worst_zoom = zooms[worst_step - 1];

    eprintln!(
        "  plate at the fit: ×{zoom_at_fit:.1}, {segments_at_fit} segments; \
         peak ×{peak:.1}; back to ×{zoom_back:.1} with {segments_back} segments \
         ({PASSES} passes, each step's floor)"
    );
    eprintln!(
        "  worst of the {STEPS} swept steps: step {worst_step} at {worst_ms:.1} ms \
         (×{worst_zoom:.1})"
    );
    eprintln!(
        "  every step's floor, in ms: {}",
        floors
            .iter()
            .map(|ms| format!("{ms:.1}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    // The sweep has to arrive, or it measured forty world views and called it a zoom.
    assert!(
        peak >= SWEEP_TARGET_ZOOM,
        "the sweep only reached ×{peak:.1}: the wheel no longer zooms the plate, so the \
         worst frame below is the cost of the world view, not of ×96"
    );
    // And it has to come back, or the forty steps were half a gesture.
    assert!(
        zoom_back <= zoom_at_fit * 1.5,
        "the sweep ended at ×{zoom_back:.1} after starting at ×{zoom_at_fit:.1}: the zoom \
         out did not return to the fit"
    );
    assert!(
        worst_ms <= ZOOM_WORST_FRAME_MS * PROFILE,
        "the worst step of the zoom sweep — step {worst_step} of {STEPS}, ×{worst_zoom:.1} \
         — took {worst_ms:.1} ms at its best of {PASSES} passes, over the {:.1} ms ceiling",
        ZOOM_WORST_FRAME_MS * PROFILE
    );
}

/// A click that lands on a pin costs one frame and returns the pin, and the window reads
/// nothing to answer it.
///
/// Two halves, because the window deliberately drops what the map answers (the map's card
/// has no place filter to hand a pin to yet, `views/overview.rs`): the plate on its own,
/// where the pin a click returns is observable, and the whole window, where the status
/// row's own read counter is.
#[test]
fn clicking_a_pin_is_one_frame_and_no_read() {
    let Some(geojson) = geojson_or_skip() else {
        return;
    };
    let Some(archive) = archive_or_skip() else {
        return;
    };
    let mut calls = 0;
    let pins = read(&archive, HOURS, "", &mut calls).pins;
    assert!(!pins.is_empty(), "the window holds no pin to click");

    // ---- the plate alone: the answer is the pin's index in `pins`
    let ctx = context();
    let mut view = MapView::new(Path::new(&geojson));
    let height = 470.0;
    let mut draw = |index: usize, at: Option<Pos2>, press: Option<bool>| {
        let mut input = frame_input(index);
        if let Some(at) = at {
            input.events.push(egui::Event::PointerMoved(at));
        }
        if let (Some(at), Some(pressed)) = (at, press) {
            input.events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        let mut picked: Option<usize> = None;
        let (spent, painted) = frame(&ctx, input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(20, 16)))
                .show(ui, |ui| {
                    picked = view.show(ui, &pins, height);
                });
        });
        (spent, painted, picked)
    };

    let (_, first, untouched) = draw(0, None, None);
    assert!(
        untouched.is_none(),
        "a plate nobody touched answered a pin: the click test would prove nothing"
    );
    let (at, radius) = first
        .dots()
        .first()
        .copied()
        .expect("the plate painted no dot to click");

    // Three clicks on the same dot. The press arms the click, the release answers it — and
    // the release frame is the one whose cost is the cost of a pin click. Three of them
    // because one frame on a machine with other agents building is one sample: measured
    // here, a single release frame reported 92 ms for work that costs 16 ms, and a budget
    // cannot be written against that.
    let mut pin_frame = Sampled::new("frame: a click that lands on a pin");
    let mut rep = None;
    let mut last: Option<Painted> = None;
    for attempt in 0..3 {
        let press = 1 + attempt * 2;
        let (_, _, armed) = draw(press, Some(at), Some(true));
        assert!(armed.is_none(), "the press answered a pin before the release");
        let (spent, painted, answered) = draw(press + 1, Some(at), Some(false));
        pin_frame.take(spent);
        match rep {
            None => rep = answered,
            Some(already) => assert_eq!(
                answered,
                Some(already),
                "the same dot answered a different pin on a second click"
            ),
        }
        last = Some(painted);
    }
    let rep = rep.expect("a click on a dot returned no pin");
    let after = last.expect("three clicks were made");
    assert!(rep < pins.len(), "the plate answered pin {rep} of {}", pins.len());
    eprintln!(
        "  clicked the dot at ({:.0}, {:.0}), radius {radius:.1} → pin {rep} “{}” \
         ({} stories), one frame at {:.2} ms (best of 3)",
        at.x,
        at.y,
        pins[rep].place,
        pins[rep].count,
        pin_frame.min()
    );

    // The click *selected* the dot, proved the way the plate says it: the dot's label is
    // painted while the pointer is on it, and it is still painted on the next frame with
    // the pointer taken off the plate (`self.selected` is what keeps it there). A label read
    // off the hover alone would prove nothing about the click.
    let label = after
        .text
        .lines()
        .find(|line| line.contains(" stories") || line.contains(" story"))
        .map(str::to_string)
        .expect("the plate painted no dot label while the pointer was on the dot");
    let (_, away, again) = draw(8, Some(Pos2::new(SCREEN.x - 4.0, SCREEN.y - 4.0)), None);
    assert!(again.is_none(), "a bare pointer move answered a pin");
    assert!(
        away.text.contains(&label),
        "the dot was picked but “{label}” is not on the plate with the pointer away — the \
         click selected nothing"
    );
    eprintln!("  the selected dot's label, with the pointer gone: “{label}”");
    pin_frame.assert_under(
        PIN_FRAME_MS * PROFILE,
        "a hit test over the drawn dots, not over the archive",
    );

    // ---- the whole window: the same click, and the read counter does not move
    let ctx = context();
    let Some(mut atlas) = window_or_skip() else {
        return;
    };
    let (_, painted) = frame(&ctx, frame_input(0), |ui| atlas.draw(ui));
    let before = painted.reads().expect("the status row painted no read count");
    let (at, _) = painted
        .dots()
        .first()
        .copied()
        .expect("the window's plate painted no dot to click");
    let (_, after) = click(&ctx, 2, at, |ui| atlas.draw(ui));
    let after_reads = after.reads().expect("the status row painted no read count");
    eprintln!("  the window: {before} read(s) before the click, {after_reads} after");
    assert_eq!(
        after_reads, before,
        "clicking a place read the archive again: {before} reads before the click, \
         {after_reads} after — a selection is not a new demand"
    );
    assert!(
        after.paints_the_plate(),
        "the window left the Overview on a click on the plate — the click landed on \
         something else"
    );
}

/// A keystroke is one read of the archive, not one read per frame — counted twice.
///
/// First against the archive directly: the eight statements `app::Atlas::reload` runs, in
/// its order and with its limits, bumped once each, so the cost of a keystroke and the
/// number of reads are measured rather than claimed. Then against the window itself: the
/// status row prints `… · N reads`, which is `Atlas`'s own counter, so a frame that read
/// would be impossible to hide. Typing is real typing — a click in the search box, then
/// `Event::Text` — not a private field set from the test.
#[test]
fn a_search_keystroke_reads_once_not_once_per_frame() {
    let Some(archive) = archive_or_skip() else {
        return;
    };

    // ---- the read a keystroke is, timed
    let mut calls = 0;
    let mut reads = Sampled::new("keystroke: one read with a needle");
    let mut empty_box = 0;
    let mut typed = 0;
    for (index, needle) in ["", "b", "be", "ber", "berl", "berli", "berlin"]
        .into_iter()
        .enumerate()
    {
        let started = Instant::now();
        let loaded = read(&archive, HOURS, needle, &mut calls);
        reads.take(started.elapsed());
        if index == 0 {
            empty_box = loaded.articles.len();
        }
        if needle == "berlin" {
            typed = loaded.articles.len();
        }
    }
    reads.print(KEYSTROKE_MS);
    assert_eq!(calls, 7 * 8, "seven keystrokes should be 56 archive calls");
    eprintln!("  \"berlin\": {typed} stories, against {empty_box} with an empty box");
    assert!(
        typed < empty_box,
        "the search answered as many rows as the empty box ({typed} of {empty_box}), so no \
         `LIKE` was evaluated and the keystroke budget measured nothing"
    );
    reads.assert_under(
        KEYSTROKE_MS * PROFILE,
        "one read of the window's articles with a needle, which is what a key press pays",
    );

    // ---- the window's own counter, through frames and a real keystroke
    if !assets::archive().is_file() {
        eprintln!("no archive — the window half of this test was skipped");
        return;
    }
    let ctx = context();
    let Some(mut atlas) = window_or_skip() else {
        return;
    };
    let (_, painted) = frame(&ctx, frame_input(0), |ui| atlas.draw(ui));
    let opened = painted.reads().expect("the status row painted no read count");
    assert_eq!(opened, 1, "the window opened without reading the archive");
    for index in 1..7 {
        let (_, painted) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        assert_eq!(
            painted.reads(),
            Some(opened),
            "a frame that changed nothing read the archive"
        );
    }

    // A page switch is **not** a read. This used to assert the opposite — "a new demand is
    // one read: opening the Articles page" — and that expectation was the defect the demand
    // split removed: every read re-queries the whole window (4 542 entities and 900 edges on
    // the shipped archive) and ends in `GraphView::fill`, which re-frames the plate. The page
    // decides what is *drawn*, not what is *queried* (`app::Demand`), so the number here is
    // zero, and it is asserted rather than assumed.
    atlas.set_page(Page::Articles);
    let mut drawn = 0;
    let (_, painted) = frame(&ctx, frame_input(7), |ui| atlas.draw(ui));
    drawn += 1;
    assert_eq!(
        painted.reads(),
        Some(opened),
        "a page switch cost a read: the page is not part of what a read is for"
    );
    for index in 8..12 {
        let (_, painted) = frame(&ctx, frame_input(index), |ui| atlas.draw(ui));
        drawn += 1;
        assert_eq!(
            painted.reads(),
            Some(opened),
            "the Articles page read the archive on a frame that changed nothing"
        );
    }

    // Typing: the search box is clicked where its own hint was painted, then one
    // character per frame — the events a keyboard sends, not a field written from here.
    let Some(box_) = painted.rect_containing("a name, a place") else {
        eprintln!(
            "the search box painted no hint — it is not empty on this archive; the typing \
             half of this test was skipped"
        );
        return;
    };
    let (_, focused) = click(&ctx, 12, box_.center(), |ui| atlas.draw(ui));
    drawn += 2;
    assert_eq!(
        focused.reads(),
        Some(opened),
        "clicking the search box cost a read: focus is not a demand"
    );
    let typing = "berlin";
    for (index, ch) in typing.chars().enumerate() {
        let mut input = frame_input(13 + index);
        input.events.push(egui::Event::Text(ch.to_string()));
        frame(&ctx, input, |ui| atlas.draw(ui));
        drawn += 1;
    }
    // The last character's read lands on the frame after it: `Atlas::sync` runs before the
    // page draws, so a demand changed during a frame is read at the start of the next one.
    let (_, settled) = frame(&ctx, frame_input(13 + typing.len()), |ui| atlas.draw(ui));
    drawn += 1;
    let after_typing = settled
        .reads()
        .expect("the status row painted no read count");
    eprintln!(
        "  the window: {opened} read at open, {} read(s) after typing {typing:?} over \
         {drawn} frames",
        after_typing
    );
    // Typing *is* a demand change, and the only one left in this walk: the page switch above
    // and the click into the box cost nothing. One read per keystroke is the ceiling; the
    // baseline is therefore `opened`, not `opened + 1`.
    assert!(
        after_typing > opened,
        "typing {typing:?} into the search box changed no demand ({after_typing} reads): \
         the click at ({:.0}, {:.0}) did not focus it",
        box_.center().x,
        box_.center().y
    );
    assert!(
        after_typing <= opened + typing.len() as u32,
        "typing {} characters cost {} reads — more than one read per keystroke",
        typing.len(),
        after_typing - opened
    );
}
