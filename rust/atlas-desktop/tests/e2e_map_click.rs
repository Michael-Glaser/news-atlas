//! Three complaints, answered end to end through the real window.
//!
//! 1. A click on a dot of the Overview map opens the newest story filed at that place:
//!    the window switches to Articles with that story selected and its text on screen.
//! 2. After the map is zoomed, a click on the dot still opens its story — the hit test
//!    follows the camera, and the point that used to hold the dot no longer does.
//! 3. Every pin the plate draws on the shipped archive, at 1280×800 and at 2400×1400, is
//!    reachable: a press on a dot's own centre answers that dot's door, and a press at a
//!    pin's own pixel names a pick that holds that pin. Nothing painted is unreachable.
//!
//! The first two tests drive `Atlas` against a temporary archive of their own. The click is
//! aimed at the pixel the window **painted** the dot at — a filled circle in the frame's
//! shapes — and the plate's rectangle is read the same way (the filled rect the plate is
//! cleared to). So the chain under test is the whole one: painted dot → synthetic press and
//! release at that point → the plate's hit test → `Pin::article` → `Atlas` switching page →
//! the reading pane's text. A test that computed the projection itself would be testing its
//! own arithmetic, and a test that called `Pin::article` directly would be testing SQL, not
//! the window.
//!
//! The third test does not drive `Atlas`: the app's first frame reads the request queue and
//! the licence state (`app.rs:523`), which are files outside the archive that an audit has no
//! business touching. It draws [`MapView::show_countries`] into a bare `egui::Context`
//! instead and presses through the same `Response`, so the hit test, the geometry and the
//! camera are still the app's own.
//!
//! The dot is identified by data, not by a remembered coordinate: it is the plate's dot whose
//! radius is the largest, because the radius is `views/map::dot_radius` of the stories behind
//! a dot and only the two-story place here has one. That survives the zoom (a dot's radius
//! never changes unless the archive does) and needs no state inside the app. The first test
//! presses *every* dot the plate shows, so a place that stops being drawn fails it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::data::{Archive, Pin};
use atlas_desktop::geo::{Camera, Geo};
use atlas_desktop::theme;
use atlas_desktop::views::map::{cluster, MapView, Pick, CLUSTER_PX};

/// The screen `src/main.rs` opens: a real 1440×900 window.
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

/// Every story's content marker, in fixture order: the reading pane paints `content`, and
/// `content` starts with one marker, so a marker on screen names the one story that is open.
const MARKERS: [&str; 6] = [
    "PANE-MADRID-NEW",
    "PANE-MADRID-OLD",
    "PANE-HAMBURG",
    "PANE-TOKYO",
    "PANE-CAPE-TOWN",
    "PANE-VANCOUVER",
];

// ---------------------------------------------------------------------------
// a temporary archive with five places, one of them carrying two stories
// ---------------------------------------------------------------------------

/// `data/atlas.db` of our own, in a temp directory, removed when the test ends.
///
/// The live archive is never opened: the story a dot opens has to be a story this file
/// wrote, or the assertion would be about the machine rather than about the window.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

/// Keeps two archives built on different test threads apart when the clock's resolution
/// is coarser than a thread switch (it is; the smoke test measured the collision).
static NEXT_ARCHIVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl TempArchive {
    fn build() -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-e2e-map-{}-{}-{}",
            std::process::id(),
            nanos,
            NEXT_ARCHIVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("atlas.db");

        let conn = rusqlite::Connection::open(&file).expect("a fresh database");
        conn.execute_batch(
            "CREATE TABLE articles(
                 id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                 published TEXT, summary TEXT, content TEXT, iso TEXT, place TEXT,
                 url TEXT, full_text INTEGER, relevant INTEGER, dupe INTEGER,
                 lat REAL, lon REAL);
             CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE entities(
                 id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER, last_seen TEXT);
             CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL, label TEXT);
             CREATE TABLE article_links(a INTEGER, b INTEGER);
             CREATE TABLE topics(
                 id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER,
                 hits INTEGER, last_hit TEXT);
             CREATE TABLE monitors(
                 id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER,
                 cadence_minutes INTEGER, last_run TEXT, hits INTEGER);
             CREATE TABLE alerts(id INTEGER PRIMARY KEY);",
        )
        .expect("the schema data.rs reads");

        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'Madrid Desk'),(2,'Elsewhere')",
            [],
        )
        .expect("two sources");

        // Five places, and Madrid carries two stories so that "the pin's newest story" is a
        // sentence with an answer: the newer one, not the older one.
        //
        // **All five are on the plate at the window `src/main.rs` opens.** The plate used to be
        // fitted to the world's *width* (1 188 px for 360° at 3.3 px/degree), so a 309 px tall
        // plate held the latitudes ±46.8° only and Hamburg (54.0 °N) and Vancouver (49.3 °N)
        // were cropped off it — the user's "die Karte wird nicht ganz angezeigt". The view now
        // seats the world in its own 2:1 box inside the card, so every latitude is on it. The
        // first test below counts the dots the plate shows and presses every one of them, so a
        // place that disappears from the plate still takes the test with it.
        //
        // `content` holds a marker per story. The reading pane paints `content` (the list
        // paints only titles and summaries), so a marker on screen means *that* story was
        // opened and not merely listed.
        let stories: [(i64, &str, &str, &str, &str, f64, f64); 6] = [
            (1, "Madrid's market opens a new hall", "Madrid", "PANE-MADRID-NEW", "MADRID", 40.4, -3.7),
            (2, "Madrid's older hall closes for repairs", "Madrid", "PANE-MADRID-OLD", "MADRID", 40.4, -3.7),
            (3, "Harbour crane strike ends after nine days", "Hamburg", "PANE-HAMBURG", "DEU", 54.0, 10.0),
            (4, "Chip export rules tighten in Asia", "Tokyo", "PANE-TOKYO", "JPN", 35.7, 139.7),
            (5, "Ferry traffic resumes on the southern route", "Cape Town", "PANE-CAPE-TOWN", "ZAF", -33.9, 18.4),
            (6, "Timber prices settle on the west coast", "Vancouver", "PANE-VANCOUVER", "CAN", 49.3, -123.1),
        ];
        for (id, title, place, marker, iso, lat, lon) in stories {
            // Madrid's two stories are the two newest, so the pin's `newest` is id 1 and
            // the older one is provably a different story.
            let hours_ago = match id {
                1 => 2,
                2 => 20,
                other => 6 + other,
            };
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 1, 0, ?10, ?11)",
                rusqlite::params![
                    id,
                    title,
                    if id == 1 || id == 2 { 1 } else { 2 },
                    stamp(hours_ago),
                    format!("{title} — a summary sentence for the list."),
                    format!("{marker}: the body of this story, long enough to fill the pane."),
                    iso,
                    place,
                    format!("https://example.invalid/{id}"),
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        drop(conn);
        TempArchive { dir, file }
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`).
fn stamp(hours_ago: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let at = now - hours_ago * 3600;
    let (days, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

// ---------------------------------------------------------------------------
// one frame of the real window, and what it painted
// ---------------------------------------------------------------------------

/// What one frame of the window drew that this test needs: the strings, the filled
/// circles (the map's dots) and the filled rectangles (the map's plate).
#[derive(Default)]
struct Frame {
    text: String,
    dots: Vec<(Pos2, f32)>,
    rects: Vec<Rect>,
}

impl Frame {
    fn paints(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    /// The plate: the rectangle the view clears to the page colour, the width of the page's
    /// card and tall enough to hold a map — the rail's own rects are 188 px wide and the
    /// cards' rules are hairlines, so width and height together are what tell them apart.
    ///
    /// The height is **not** a constant any more, and that is the point: the plate is the
    /// card's width over [`atlas_desktop::views::overview::PLATE_ASPECT`], clamped to the
    /// room the page has, so it is 490 px in a 1440x900 window and 171 px in the smallest
    /// supported one. The old filter looked for a 470 px rectangle and panicked
    /// ("painted no 470 px plate") the day the height became measured. What identifies the
    /// plate now is that it is the *smallest* wide, tall rectangle that has a dot on it: the
    /// card around it also holds the dots and is larger, and nothing else in the window is
    /// 800 px wide with a place painted inside it.
    fn plate(&self) -> Rect {
        self.plate_if_it_holds_a_dot().unwrap_or_else(|| {
            panic!(
                "the frame painted no plate with a place on it; its rectangles were {:?} \
                 and its dots {:?}",
                self.rects, self.dots
            )
        })
    }

    /// [`Frame::plate`] without the panic: a zoom can carry every place off the plate, and a
    /// frame that shows none is a change in the dot list, not a broken frame.
    fn plate_if_it_holds_a_dot(&self) -> Option<Rect> {
        let holds_a_dot = |rect: &Rect| self.dots.iter().any(|(at, _)| rect.contains(*at));
        self.rects
            .iter()
            .copied()
            .filter(|rect| rect.width() > 800.0 && rect.height() > 150.0 && holds_a_dot(rect))
            .min_by(|left, right| {
                (left.width() * left.height())
                    .partial_cmp(&(right.width() * right.height()))
                    .expect("two finite areas")
            })
    }

    /// The dots the plate shows, in paint order — empty when it shows none.
    fn dots_set(&self) -> Vec<Pos2> {
        self.plate_if_it_holds_a_dot()
            .map(|plate| {
                self.dots_on(plate)
                    .into_iter()
                    .map(|(at, _)| at)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The dots drawn *on the plate*. The rail's logo is a filled circle too, and it is
    /// not a place: the plate's rectangle is what tells a pin from the mark.
    fn dots_on(&self, plate: Rect) -> Vec<(Pos2, f32)> {
        self.dots
            .iter()
            .copied()
            .filter(|(at, _)| plate.contains(*at))
            .collect()
    }

    /// The dot that stands for the place this test filed two stories at: the biggest one,
    /// because the radius is the view's own function of the stories behind a dot
    /// (`map::dot_radius`), and it is the only dot of its size here.
    ///
    /// Identified by data rather than by geometry on purpose: the dot's position moves
    /// under a zoom and its neighbours' distances from the middle of the plate do not
    /// order the same way at every scale (measured: Cape Town, not Madrid, is the dot
    /// nearest the middle), while the radius never changes unless the archive does.
    fn the_two_story_dot(&self, plate: Rect) -> (Pos2, f32) {
        let dots = self.dots_on(plate);
        let (at, radius) = dots
            .iter()
            .copied()
            .max_by(|left, right| left.1.partial_cmp(&right.1).expect("two finite radii"))
            .unwrap_or_else(|| panic!("the plate painted no dots; text was:\n{}", self.text));
        let as_big = dots.iter().filter(|(_, other)| *other > radius - 0.01).count();
        assert_eq!(
            as_big, 1,
            "two dots are the same size, so 'the dot with two stories' is ambiguous: {dots:?}"
        );
        (at, radius)
    }

    /// The zoom the plate's own caption reports, read back out of what it painted.
    fn zoom(&self) -> f32 {
        let caption = self
            .text
            .lines()
            .find(|line| line.starts_with("zoom ×"))
            .unwrap_or_else(|| panic!("the plate painted no zoom caption:\n{}", self.text));
        caption["zoom ×".len()..]
            .split_whitespace()
            .next()
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("the caption's zoom is not a number: {caption:?}"))
    }
}

fn record(shapes: &[egui::epaint::ClippedShape]) -> Frame {
    let mut frame = Frame::default();
    for clipped in shapes {
        walk(&clipped.shape, &mut frame);
    }
    frame
}

fn walk(shape: &Shape, frame: &mut Frame) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, frame);
            }
        }
        Shape::Text(text) => {
            frame.text.push_str(text.galley.text());
            frame.text.push('\n');
        }
        // A filled circle is a dot on the plate; a stroked one is a cluster's ring or a
        // focus ring, and `fill.a() == 0` is what tells them apart.
        Shape::Circle(circle) if circle.fill.a() > 0 => {
            frame.dots.push((circle.center, circle.radius));
        }
        Shape::Rect(rect) => frame.rects.push(rect.rect),
        _ => {}
    }
}

/// The marker of the story the reading pane holds, if it holds one: `content` starts with
/// the story's marker, and the list paints titles and summaries rather than bodies.
fn open_marker(frame: &Frame) -> Option<&'static str> {
    MARKERS.into_iter().find(|marker| frame.paints(marker))
}

/// The input of one frame: a screen, a clock that moves, and whatever pointer or wheel
/// events the caller wants.
fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        events,
        ..Default::default()
    }
}

/// One frame of the real window, with the app's own style installed.
fn frame(ctx: &egui::Context, atlas: &mut Atlas, events: Vec<egui::Event>) -> Frame {
    let mut output = ctx.run_ui(input(events), |ui| atlas.draw(ui));
    // The texture deltas are the backend's business, and `epaint` debug-asserts that they
    // were handled rather than dropped: `clear` is the documented way to say "no backend
    // here", and it is what makes a hundred frames of a window affordable in a test.
    output.textures_delta.clear();
    record(&output.shapes)
}

/// A context styled as the app styles it (`Atlas::new` does the same, without a window).
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// Press and release the primary button at `at`, then draw the frame after it — the frame
/// in which a page switch becomes visible — and return that frame's painting.
///
/// `egui` reports `Response::clicked()` on the release at the point the press happened,
/// so a click is two frames; the window sets its page inside the release frame and draws
/// that page from the next one.
fn click(ctx: &egui::Context, atlas: &mut Atlas, at: Pos2) -> Frame {
    for pressed in [true, false] {
        frame(
            ctx,
            atlas,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    frame(ctx, atlas, vec![egui::Event::PointerMoved(at)])
}

/// Two wheel events over `at`, `points` each — one plain (the plate zooms on a wheel, see
/// `MapView::show`), one with Ctrl held (egui turns that into a pinch, which the same code
/// path adds to the zoom) — then wait for the plate to settle and answer that frame.
///
/// `points` is a notch, and its size is a measured decision: 40 points twice settles at
/// ×1.4, which on this window's 309 px plate carries every place off it (measured: the last
/// dot, Cape Town, sits at y 443.8 and the plate ends at 445) and leaves nothing to press;
/// 10 points twice settles at ×1.1 with all four dots still on the plate. A test that
/// emptied the plate would measure an empty plate.
///
/// The pointer is where the zoom is anchored: `views/map.rs` reads `response.hover_pos()` and
/// hands it to `zoom_by`/`pinch`, so pointing at `at` is what makes the plate zoom *there*.
/// A reader who wants a place closer puts the pointer on it, which is why this is the honest
/// gesture rather than a camera the test moved itself.
///
/// Settling is done on the **dots' own positions** — every dot on the plate, as a list — and
/// not on the caption: `egui` smooths the wheel, so the plate keeps consuming a decaying
/// delta for a while after the events, and the caption's one decimal place looked settled
/// while a dot was still moving 12 px between frames (measured). A click aimed at a moving
/// dot is a race, and this is the frame that stops it. The whole list rather than one dot,
/// because a zoom may carry a dot off the plate and a shorter list is a change too.
fn wheel(ctx: &egui::Context, atlas: &mut Atlas, at: Pos2, points: f32) -> Frame {
    let wheel_event = |delta: f32, modifiers: egui::Modifiers| {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, delta),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers,
            },
        ]
    };
    frame(ctx, atlas, wheel_event(points, egui::Modifiers::default()));
    frame(ctx, atlas, wheel_event(points, egui::Modifiers::CTRL));
    let mut last: Option<Vec<Pos2>> = None;
    let mut still = 0;
    for _ in 0..300 {
        let painted = frame(ctx, atlas, vec![egui::Event::PointerMoved(at)]);
        let now = painted.dots_set();
        if let Some(previous) = &last {
            if !now.is_empty()
                && now.len() == previous.len()
                && now
                    .iter()
                    .zip(previous)
                    .all(|(moved, was)| moved.distance(*was) < 0.05)
            {
                still += 1;
            } else {
                still = 0;
            }
        }
        last = Some(now);
        if still >= 5 {
            return painted;
        }
    }
    panic!("the plate never stopped moving after two wheel events");
}

/// A window on this file's archive, showing the Overview, with its first frame drawn.
fn window() -> (TempArchive, egui::Context, Atlas) {
    let temp = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.file.clone());
    assert_eq!(
        atlas.page(),
        Page::Overview,
        "the window must open on the page with the map"
    );
    let painted = frame(&ctx, &mut atlas, Vec::new());
    assert!(
        painted.paints("World map"),
        "the overview painted no map card:\n{}",
        painted.text
    );
    (temp, ctx, atlas)
}

// ---------------------------------------------------------------------------
// (1) a dot opens its place's newest story
// ---------------------------------------------------------------------------

/// Clicking a dot on the Overview map opens that place's newest story: the window is on
/// Articles, the reading pane holds the newest of the two stories filed at Madrid, and it
/// does not hold the older one.
#[test]
fn clicking_a_dot_opens_the_newest_story_filed_there() {
    let (_temp, ctx, mut atlas) = window();
    let painted = frame(&ctx, &mut atlas, Vec::new());
    let plate = painted.plate();
    let dots = painted.dots_on(plate);
    // Every place the fixture files is on the plate at ×1 — the whole world is, now that the
    // fit contains it — and each of the five has one dot. The count is asserted so that a place
    // that quietly leaves the plate, a pin the view stops drawing, or a second dot on one place
    // fails here rather than passing as "fewer dots".
    assert_eq!(
        dots.len(),
        5,
        "all five places are on the plate at ×1 and each must have one dot; Madrid's dot is \
         the two-story one: {dots:?}"
    );
    let (madrid, radius) = painted.the_two_story_dot(plate);
    assert!(
        radius > 2.0,
        "the dot has no size to aim at: {radius} px at {madrid:?}"
    );

    // Every dot, pressed: each opens the newest story filed at its own place, and Madrid's
    // dot opens the newer of its two. Read back through the pane's marker, so a dot that
    // opened the list but no story fails here.
    let mut opened: Vec<&str> = Vec::new();
    for (at, _) in &dots {
        let after = click(&ctx, &mut atlas, *at);
        assert_eq!(
            atlas.page(),
            Page::Articles,
            "a click on the dot at {at:?} did not switch the window to Articles:\n{}",
            after.text
        );
        let marker = open_marker(&after)
            .unwrap_or_else(|| panic!("the dot at {at:?} opened no story:\n{}", after.text));
        if marker == "PANE-MADRID-NEW" {
            // …and the title is on screen once the story is open, in the list and the pane.
            assert!(
                after.paints("Madrid's market opens a new hall"),
                "the opened story's title was not painted:\n{}",
                after.text
            );
        }
        opened.push(marker);
        // Back to the plate for the next dot: the map is the page this test is about.
        atlas.set_page(Page::Overview);
        for _ in 0..12 {
            frame(&ctx, &mut atlas, Vec::new());
        }
    }
    opened.sort_unstable();
    assert_eq!(
        opened.as_slice(),
        [
            "PANE-CAPE-TOWN",
            "PANE-HAMBURG",
            "PANE-MADRID-NEW",
            "PANE-TOKYO",
            "PANE-VANCOUVER",
        ],
        "the five places of the fixture are on the plate — Madrid, Cape Town and Tokyo, and \
         Hamburg and Vancouver, which the old width fit cropped — and each opens its newest \
         story; Madrid's is the newer of its two, never PANE-MADRID-OLD"
    );
}

// ---------------------------------------------------------------------------
// (2) the hit test follows the camera
// ---------------------------------------------------------------------------

/// After two wheel events over the plate, the dot has moved; the point it used to occupy
/// is water and a click there opens nothing; the dot's new position opens the same story.
#[test]
fn after_a_zoom_the_dot_still_opens_its_story() {
    let (_temp, ctx, mut atlas) = window();
    let before = frame(&ctx, &mut atlas, Vec::new());
    let plate = before.plate();
    let (old_at, _) = before.the_two_story_dot(plate);
    let zoom_before = before.zoom();
    assert!(
        (zoom_before - 1.0).abs() < 0.05,
        "the plate must open at its fit zoom, not at ×{zoom_before}"
    );

    // The zoom is anchored at the pointer (`views/map.rs` hands `hover_pos` to `zoom_by`), so
    // this aims it a fifth of the plate's height down from the top — about 40 px under the
    // dot. Aimed at the plate's middle instead, ×1.23 carries Madrid's dot — 21 px under the
    // plate's top edge at the fit — off the plate (measured: y 157.2 → 126.3, ten pixels
    // above the edge), and the test would be measuring an empty plate rather than the hit
    // test. A reader zooms just under the place they want closer, which is the same gesture.
    let anchor = Pos2::new(old_at.x, plate.top() + plate.height() * 0.2);
    let zoomed = wheel(&ctx, &mut atlas, anchor, 40.0);
    let zoom_after = zoomed.zoom();
    assert!(
        zoom_after > zoom_before + 0.1,
        "two wheel events over the plate did not zoom it: ×{zoom_before} -> ×{zoom_after}"
    );
    let (new_at, _) = zoomed.the_two_story_dot(zoomed.plate());
    assert!(
        new_at.distance(old_at) > 6.0,
        "the dot did not move under the zoom (×{zoom_before} -> ×{zoom_after}): \
         {old_at:?} -> {new_at:?}, so the hit test was not exercised"
    );

    // (a) The dot's new position is the dot: the press opens the story that moved there.
    let after = click(&ctx, &mut atlas, new_at);
    assert_eq!(
        atlas.page(),
        Page::Articles,
        "after a zoom, a click on the dot at {new_at:?} did not open its story:\n{}",
        after.text
    );
    assert!(
        after.paints("PANE-MADRID-NEW"),
        "the zoomed plate's dot opened the wrong story:\n{}",
        after.text
    );
    assert!(
        !after.paints("PANE-MADRID-OLD"),
        "the zoomed plate's dot opened the older story as well:\n{}",
        after.text
    );

    // (b) The position the dot held before the zoom is not the dot any more. It is still
    // *land*, and land has been a door of its own since countries became clickable
    // (`views/map.rs::Pick::Country` → `Choice::at_country`), so the promise this asserts is
    // the one the hit test makes: a press where the dot used to be never reaches the story
    // that moved away. The earlier form of this test asked for `Page::Overview` here, which
    // was true only while a click on a country did nothing at all — the defect the window was
    // told to fix — so the expectation is widened, not dropped.
    atlas.set_page(Page::Overview);
    for _ in 0..12 {
        frame(&ctx, &mut atlas, Vec::new());
    }
    let missed = click(&ctx, &mut atlas, old_at);
    assert!(
        !missed.paints("PANE-MADRID-NEW"),
        "a click at the dot's pre-zoom position opened the dot's story anyway — the hit test \
         is not following the camera:\n{}",
        missed.text
    );
    assert!(
        !missed.paints("PANE-MADRID-OLD"),
        "a click at the dot's pre-zoom position opened Madrid's older story:\n{}",
        missed.text
    );
}

/// The zoom really is the camera's, not a number the caption invents: the same archive,
/// the same dots, a different scale — the distances between dots grow with it.
#[test]
fn the_zoom_scales_the_plate_that_the_dots_are_hit_on() {
    let (_temp, ctx, mut atlas) = window();
    let before = frame(&ctx, &mut atlas, Vec::new());
    let plate = before.plate();

    // How far the dots are spread on the plate: the farthest dot's radius from the middle.
    // A scale-free measure, so it survives any dot leaving the plate.
    let spread = |frame: &Frame| {
        let centre = frame.plate().center();
        frame
            .dots_on(frame.plate())
            .iter()
            .map(|(at, _)| at.distance(centre))
            .fold(0.0_f32, f32::max)
    };
    let spread_before = spread(&before);

    // About the plate's middle: this test is about the scale the dots are spread by, and
    // a dot leaving the plate does not matter to a scale-free measure.
    let after = wheel(&ctx, &mut atlas, plate.center(), 10.0);
    let spread_after = spread(&after);
    assert!(
        spread_after > spread_before * 1.05,
        "the plate zoomed ×{} -> ×{} but its dots did not spread: {spread_before:.1} px -> \
         {spread_after:.1} px",
        before.zoom(),
        after.zoom()
    );
}


// ---------------------------------------------------------------------------
// (3) every pin the plate draws, at two window sizes, answers its own story
// ---------------------------------------------------------------------------

/// The window the map opens on.
const AUDIT_HOURS: i64 = 72;

/// The two windows the plate is measured in: the narrowest page it must still work at, and a
/// large one. The plate is given the window's whole height here, which is the roomiest box it
/// can ever be drawn in; the card's own plate is shorter and therefore narrower in longitude,
/// so a pin that lands on this box lands on that one too.
const PLATES: [(f32, f32); 2] = [(1280.0, 800.0), (2400.0, 1400.0)];

/// The shipped archive, read-only, or a sentence saying there is none.
fn live_archive() -> Option<Archive> {
    let path = atlas_desktop::assets::archive();
    if !path.is_file() {
        eprintln!("no archive at {} — nothing to press", path.display());
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some(archive),
        Err(sentence) => {
            eprintln!("{sentence} — nothing to press");
            None
        }
    }
}

/// One headless plate: [`MapView::show_countries`] drawn into a bare `egui::Context` at a fixed
/// size, with presses delivered through `Response` like a reader's. No window is opened.
struct Screen {
    ctx: egui::Context,
    size: Vec2,
    time: f64,
}

impl Screen {
    fn new(size: Vec2) -> Screen {
        let screen = Screen {
            ctx: egui::Context::default(),
            size,
            time: 0.0,
        };
        theme::install(&screen.ctx);
        screen
    }

    fn draw(
        &mut self,
        events: Vec<egui::Event>,
        view: &mut MapView,
        pins: &[Pin],
        countries: &[(String, i64)],
    ) -> Option<usize> {
        let height = self.size.y;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        let mut answered = None;
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    answered = view.show_countries(ui, pins, countries, height);
                });
        });
        output.drop_without_applying_deltas();
        answered
    }

    /// Frames until the camera stops easing, so a press lands where the dots were painted.
    fn settle(&mut self, view: &mut MapView, pins: &[Pin], countries: &[(String, i64)]) {
        let mut last = (view.camera().zoom(), view.camera().offset());
        let mut still = 0;
        for _ in 0..120 {
            self.draw(Vec::new(), view, pins, countries);
            let now = (view.camera().zoom(), view.camera().offset());
            let calm = (now.0 - last.0).abs() < 1e-5
                && (now.1.x - last.1.x).abs() < 0.01
                && (now.1.y - last.1.y).abs() < 0.01;
            still = if calm { still + 1 } else { 0 };
            if still >= 2 {
                return;
            }
            last = now;
        }
    }

    /// Press and release at `at`: the gesture a reader makes on a dot.
    fn click(
        &mut self,
        at: Pos2,
        view: &mut MapView,
        pins: &[Pin],
        countries: &[(String, i64)],
    ) -> Option<usize> {
        self.draw(vec![egui::Event::PointerMoved(at)], view, pins, countries);
        let mut events = vec![egui::Event::PointerMoved(at)];
        events.extend(pointer(at, true));
        self.draw(events, view, pins, countries);
        let mut events = vec![egui::Event::PointerMoved(at)];
        events.extend(pointer(at, false));
        self.draw(events, view, pins, countries)
    }
}

fn pointer(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }]
}

/// Where the plate painted a pin, from the view's own camera: the same projection the plate
/// used, not a second copy of it.
fn painted(camera: Camera, rect: Rect, pin: &Pin) -> Pos2 {
    let point = camera.project(Geo::new(pin.lon as f32, pin.lat as f32));
    Pos2::new(rect.center().x + point.x, rect.center().y + point.y)
}

/// The window counted straight from the file, read-only, in `pins`' own terms: every story the
/// Articles page would list, the ones `pins` can draw, and the ones it cannot place — a row
/// without a coordinate, or with one and no name to group it by (0°/0° is the archive saying
/// "no coordinate at all", so it is not a place either).
///
/// Three numbers, one statement each; the SQL mirrors `Archive::pins` deliberately, because
/// the point of the measure is to say how many stories the plate *could* draw and does not.
fn window_counts(hours: i64) -> (i64, i64, i64) {
    let path = atlas_desktop::assets::archive();
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the shipped archive opens read-only");
    let since = stamp(hours);
    let count = |sql: &str| -> i64 {
        conn.query_row(sql, rusqlite::params![since], |row| row.get(0))
            .unwrap_or(0)
    };
    let total = count(
        "SELECT count(*) FROM articles WHERE relevant = 1 AND dupe = 0 AND published >= ?1",
    );
    let located = count(
        "SELECT count(*) FROM articles WHERE relevant = 1 AND dupe = 0 AND published >= ?1 \
           AND lat IS NOT NULL AND lon IS NOT NULL AND NOT (lat = 0 AND lon = 0) \
           AND lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180",
    );
    let drawn = count(
        "SELECT count(*) FROM articles WHERE relevant = 1 AND dupe = 0 AND published >= ?1 \
           AND lat IS NOT NULL AND lon IS NOT NULL AND NOT (lat = 0 AND lon = 0) \
           AND lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180 \
           AND coalesce(nullif(place, ''), nullif(iso, ''), '') <> ''",
    );
    (total, located, drawn)
}

/// The pins a pick stands for: one index for a single place, the dot's members for a merged
/// dot, nothing for land with no dot on it.
fn chosen(pick: Option<&Pick>) -> Option<Vec<usize>> {
    match pick? {
        Pick::Place(index) => Some(vec![*index]),
        Pick::Places(members) => Some(members.clone()),
        Pick::Country(_) => None,
    }
}

/// 3. Every pin the plate draws is reachable with a press at its own pixel, and a press on a
/// dot's centre answers that dot's own story — at both window sizes.
///
/// The one line printed per size is the promise: how many stories the window holds, how many
/// carry a coordinate, how many of those carry a name to group them by, how many pins and dots
/// that makes, and how many of the pins press exactly the dot they were painted as. A pin is a
/// place, not a story: a dot that merges several places answers with its member list, and the
/// press at a member's own pixel must name a pick whose set holds that member — the story is
/// then one click away in the list, which is `map_clusters.rs`' audit. What must never happen is
/// a painted pin that no press reaches.
///
/// The plate is the window's own box here, not the 2.1:1 card the Overview allocates: it is the
/// largest box the view is ever drawn in, so every pin pressable at 1280×800 and 2400×1400 is
/// pressable on the shorter card too — a smaller plate drops pins, it never invents one.
///
/// Measured on the shipped archive, 2026-11-22, 72 h window: 242 stories, 227 with a coordinate,
/// 227 of those named — 32 pins, 56 countries. Both sizes painted all 32 pins in 23 dots, all
/// 23 answering a press with a story of the archive, and all 32 pins pressing exactly their own
/// dot.
#[test]
fn every_pin_the_plate_draws_answers_its_own_story() {
    let Some(archive) = live_archive() else { return };
    let pins = archive.pins(AUDIT_HOURS, atlas_desktop::app::LIST_ROWS);
    let countries = archive.countries(AUDIT_HOURS, atlas_desktop::app::COUNTRY_ROWS);
    let stories: i64 = pins.iter().map(|pin| pin.count).sum();
    let (total, located, drawn) = window_counts(AUDIT_HOURS);
    let unnamed = located - drawn;
    // The one number that can make this file prove nothing: a window with no placeable story.
    // It is not an assertion, because an archive older than the window is not a defect of the
    // plate — the sentence is printed and the test says so instead of passing silently.
    if drawn == 0 {
        eprintln!(
            "{total} stories in {AUDIT_HOURS} h, none with a name and a coordinate — nothing to \
             press"
        );
        return;
    }
    // A pin is a place: the pins must account for every row the plate can draw, exactly once,
    // or a story with a coordinate would be reachable nowhere.
    assert_eq!(
        stories,
        drawn,
        "the {} pins count {stories} stories and {drawn} rows of the window have a name and a \
         coordinate: the plate loses {}",
        pins.len(),
        drawn - stories
    );
    assert!(
        pins.iter().all(|pin| pin.article > 0),
        "a pin without a destination story"
    );

    let geojson = atlas_desktop::assets::geojson();

    for (width, height) in PLATES {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height));
        let mut screen = Screen::new(Vec2::new(width, height));
        let mut view = MapView::new(&geojson);
        screen.settle(&mut view, &pins, &countries);
        let camera = view.camera();
        let dots = cluster(&pins, |pin| painted(camera, rect, pin), rect, CLUSTER_PX);

        // 1. Clustering never drops a pin that is inside the box, and never counts one twice.
        let mut seen = vec![0_usize; pins.len()];
        for dot in &dots {
            for member in &dot.members {
                seen[*member] += 1;
            }
        }
        let drawn_pins = pins
            .iter()
            .filter(|pin| rect.contains(painted(camera, rect, pin)))
            .count();
        assert_eq!(
            seen.iter().filter(|count| **count > 0).count(),
            drawn_pins,
            "at {width}×{height}: {drawn_pins} pins are inside the plate but {} are in a dot",
            seen.iter().filter(|count| **count > 0).count()
        );
        assert!(
            seen.iter().all(|count| *count <= 1),
            "at {width}×{height}: a pin is in more than one dot"
        );

        // 2. A press on a dot's own centre answers that dot's door — the heaviest pin behind it
        //    that still has a story, `views/map.rs::door` — and names exactly the members the
        //    dot was built from. The page turns that index into `pins[index].article`, so the
        //    id checked against the archive below is the one a click really opens.
        let mut opened = Vec::new();
        for dot in &dots {
            let answered = screen.click(dot.at, &mut view, &pins, &countries);
            let wanted = dot
                .members
                .iter()
                .copied()
                .find(|member| pins[*member].article > 0);
            assert_eq!(
                answered,
                wanted,
                "at {width}×{height}: a press on the dot at {:?} ({} pins, {} stories) answered \
                 {answered:?} instead of pin {wanted:?}",
                dot.at,
                dot.members.len(),
                dot.stories
            );
            let members = chosen(view.picked());
            if dot.members.len() == 1 {
                assert_eq!(
                    members,
                    Some(vec![dot.rep]),
                    "at {width}×{height}: the dot of one pin did not answer Pick::Place"
                );
            } else {
                assert_eq!(
                    members.as_deref(),
                    Some(dot.members.as_slice()),
                    "at {width}×{height}: a merged dot answered a different member list"
                );
            }
            let story = answered.map(|index| pins[index].article).unwrap_or(0);
            assert!(
                story > 0,
                "at {width}×{height}: the dot at {:?} has no live story behind it",
                dot.at
            );
            opened.push(story);
        }
        let ids = archive.articles_by_ids(&opened);
        assert_eq!(
            ids.len(),
            opened.len(),
            "at {width}×{height}: {} of {} answered story ids are not in the archive",
            opened.len() - ids.len(),
            opened.len()
        );

        // 3. A press at a pin's own pixel still names that pin. Merged dots may answer with
        //    their member list; the pin must be in it, or the dot is drawn where nothing can
        //    reach it.
        let mut missed = Vec::new();
        let mut exact = 0_usize;
        for (index, dot) in dots.iter().flat_map(|dot| {
            dot.members.iter().map(move |member| (*member, dot))
        }) {
            screen.click(painted(camera, rect, &pins[index]), &mut view, &pins, &countries);
            match chosen(view.picked()) {
                Some(members) if members.contains(&index) => {
                    if members == dot.members {
                        exact += 1;
                    }
                }
                other => missed.push((
                    pins[index].place.clone(),
                    painted(camera, rect, &pins[index]),
                    dot.at.distance(painted(camera, rect, &pins[index])),
                    dot.radius,
                    other,
                )),
            }
        }
        if !missed.is_empty() {
            eprintln!(
                "at {width}×{height}: {} of {drawn} pins answer no dot of their own: {missed:?}",
                missed.len()
            );
        }
        assert!(
            missed.is_empty(),
            "at {width}×{height}: {} painted pins answer nothing",
            missed.len()
        );

        println!(
            "{width:.0}×{height:.0}: {total} stories in {AUDIT_HOURS} h, {located} with a \
             coordinate, {drawn} of those with a name ({unnamed} placed but unnamed) — \
             {pins} pins for {stories} stories, {countries} countries; {on_plate} pins on the \
             plate in {dots} dots, every dot answering a press with a story, {exact} of \
             {on_plate} pins pressing exactly their own dot",
            pins = pins.len(),
            countries = countries.len(),
            on_plate = drawn_pins,
            dots = dots.len(),
        );
    }
}
