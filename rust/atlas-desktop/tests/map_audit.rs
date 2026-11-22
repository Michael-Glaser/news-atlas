//! The map's interaction audit, against the real archive: does every dot open a story, and is
//! that story the one the place is about?
//!
//! Three promises the Overview map makes, audited here on `data/atlas.db` read-only:
//!
//! 1. **A dot is a door.** A click on a dot asks the page for a story id
//!    (`views/map.rs::show` → the pin index, `views/overview.rs::map_card` → `Choice.story`),
//!    and the id must be a row that exists in the archive. Checked on **every** dot the plate
//!    of the shipped archive paints, and on the **newest 50 located articles** — the ones a
//!    reader is most likely to click.
//! 2. **The dot is where the story happened.** A pin's coordinate is the mean of the
//!    coordinates of the stories filed at that place (`data.rs::pins`), so the audit
//!    regroups the same window by `(label, iso)` from the raw rows and asserts the centroid
//!    matches to 1e-4 — the dot and the place are the same point, and the story a click opens
//!    is filed at that point's place.
//! 3. **Clustering never drops a marker.** Every pin whose projected point is on the plate is
//!    in exactly one dot: the dots' `places` sum to the number of on-plate pins and their
//!    `stories` sum to those pins' counts, at the fit and at two zoom levels.
//!
//! # What this file found
//!
//! Two doors on the Overview map do not do what the page says they do. Both are stated below
//! as bounds and both fail on the shipped archive; both are one-file-short of a fix in files
//! this audit does not own.
//!
//! 1. **A dot that stands for several places is not a list.** `Cluster` carries `rep` and
//!    counts, not its members, so the busiest merged dot of the shipped archive — 3 places,
//!    99 stories, drawn with a ring — reports one pin and the page opens **one** story. The
//!    other 98 are not reachable from the map at all.
//!    [`a_dot_for_several_places_opens_only_one_of_them`] states it; the fix belongs in
//!    `views/map.rs` (a cluster must be able to name its pins) and `views/overview.rs` (the
//!    card must open that list).
//! 2. **A country chip's count and its list disagree.** The chips are the *country* rollup
//!    (`Archive::countries`, grouped by `iso`), but the page reported a chip as a **place** and
//!    the window opened `Archive::place_stories`, which matches the place *label*
//!    (`coalesce(nullif(place, ''), nullif(iso, ''), '')`). Measured: the `USA` chip says 105
//!    stories, `country_stories` holds 105, and the list the click opened held **0**.
//!    [`the_place_list_a_click_produces_opens_its_articles`] stated it; it now passes because
//!    the door carries its kind (`overview::Door`, `app::stories_for`) and the test reads
//!    through that one hand-off rather than through a copy of the old query. The marker is
//!    kept here so the finding has a name.
//!
//! # How the audit reaches the code
//!
//! The page is the shipped one: `views::overview::show_choices` is drawn in a headless
//! `egui::Context`, the dot to click is the circle the frame **painted** (a filled circle on
//! the plate), and the answer is the `Choice` the page returned. Dot identity is checked
//! against `views::map::cluster` with the view's own camera, so a painted dot that does not
//! correspond to a cluster fails before any click is aimed at it.
//!
//! No window, no socket, no fixture: `data/atlas.db` is opened twice, read-only, once through
//! `Archive::open` (the code under test) and once by this file for the row-level questions
//! `Article` cannot answer — it carries no coordinate.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Pos2, Rect, Shape, Vec2};
use rusqlite::{Connection, OpenFlags};

use atlas_desktop::app::{stories_for, LIST_ROWS};
use atlas_desktop::assets;
use atlas_desktop::data::{Archive, Article, Counts, Pin};
use atlas_desktop::geo::{Camera, Geo};
use atlas_desktop::views::map::{cluster, Cluster, Pick, CLUSTER_PX};
use atlas_desktop::views::nf;
use atlas_desktop::views::overview::{self, Choice, Door, Kind, Overview, Snapshot};

/// The window the map card reports, and the one the archive is read with.
const HOURS: i64 = 72;
/// The window `src/main.rs` opens: a 1440 × 900 screen.
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);
/// How many of the newest located stories the audit opens one by one.
const SAMPLE: usize = 50;
/// The dots `data.rs::pins` is asked for: the app's own limit.
const PINS: i64 = 400;

// ---------------------------------------------------------------------- the live archive

/// The shipped archive, opened twice and read-only both times, or a sentence.
///
/// `Archive::open` is the code under test and refuses to write (`SQLITE_OPEN_READ_ONLY` and
/// `PRAGMA query_only`); this file's own connection is opened with the same flag, because the
/// questions it asks — where a row's coordinate is, what the `iso` of a label is — are not
/// answered by any public function.
fn live() -> Option<(Archive, Connection)> {
    let path = assets::archive();
    if !path.is_file() {
        println!(
            "no {} in this checkout: the map audit reads the shipped archive and skips",
            path.display()
        );
        return None;
    }
    let archive = match Archive::open(&path) {
        Ok(archive) => archive,
        Err(problem) => {
            println!("cannot read {}: {problem}", path.display());
            return None;
        }
    };
    let conn = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(problem) => {
            println!("cannot open {} read-only: {problem}", path.display());
            return None;
        }
    };
    Some((archive, conn))
}

/// One located story of the window, as the archive stores it.
///
/// The label is `data.rs::pins`' own expression — `coalesce(nullif(place, ''), nullif(iso,
/// ''), '')` — written out again because a group key that differed from the query's would
/// make every number below about this file's arithmetic instead of the archive's.
#[derive(Clone, Debug)]
struct Row {
    id: i64,
    label: String,
    iso: String,
    lat: f64,
    lon: f64,
    published: String,
}

/// The window's located rows, newest first, with the bound taken from the same clock the
/// archive uses.
const ROWS_SQL: &str = " \
    SELECT id, coalesce(nullif(place, ''), nullif(iso, ''), '') AS label, coalesce(iso, '') AS iso, \
           lat, lon, published \
    FROM articles \
    WHERE relevant = 1 AND dupe = 0 AND published >= ?1 \
      AND lat IS NOT NULL AND lon IS NOT NULL \
      AND NOT (lat = 0 AND lon = 0) \
      AND lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180 \
    ORDER BY published DESC, id DESC";

/// The window's start, in the archive's own `YYYY-MM-DDTHH:MM:SSZ` format.
fn window_bound(conn: &Connection) -> String {
    let modifier = format!("-{HOURS} hours");
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        rusqlite::params![modifier],
        |row| row.get(0),
    )
    .expect("strftime answers on the archive's own database")
}

fn window_rows(conn: &Connection, bound: &str) -> Vec<Row> {
    let mut statement = conn.prepare(ROWS_SQL).expect("the window's rows");
    let rows = statement
        .query_map(rusqlite::params![bound], |row| {
            Ok(Row {
                id: row.get(0)?,
                label: row.get(1)?,
                iso: row.get(2)?,
                lat: row.get(3)?,
                lon: row.get(4)?,
                published: row.get(5)?,
            })
        })
        .expect("the window's rows");
    rows.flatten().collect()
}

/// The pins and the rows of one window, read so that both describe the **same** window.
///
/// `pins` takes the clock's bound internally, so a second crossing between the two reads would
/// make one row of difference — and every sum below would be off by one. The pins are therefore
/// read before and after the rows and the pair is kept only when the two agree; three attempts
/// is more than a second of slack.
fn window_of(archive: &Archive, conn: &Connection) -> (Vec<Pin>, Vec<Row>) {
    for _ in 0..3 {
        let before = archive.pins(HOURS, PINS);
        let bound = window_bound(conn);
        let rows = window_rows(conn, &bound);
        let after = archive.pins(HOURS, PINS);
        if signature(&before) == signature(&after) {
            return (before, rows);
        }
    }
    panic!("the pins changed twice between two reads: this machine's clock is moving");
}

fn signature(pins: &[Pin]) -> Vec<(String, i64, i64)> {
    pins.iter()
        .map(|pin| (pin.place.clone(), pin.count, pin.article))
        .collect()
}

// ------------------------------------------------------------------------- the page

/// What one frame painted, as far as this file can see it: the plate's rectangle, every filled
/// circle on it (a stroked circle is a cluster's ring or a focus ring, not a dot), and every
/// string with the box it was painted in.
#[derive(Default)]
struct Painted {
    rects: Vec<Rect>,
    dots: Vec<(Pos2, f32)>,
    labels: Vec<(String, Rect)>,
}

impl Painted {
    fn record(shapes: &[egui::epaint::ClippedShape]) -> Painted {
        let mut painted = Painted::default();
        for clipped in shapes {
            painted.walk(&clipped.shape);
        }
        painted
    }

    fn walk(&mut self, shape: &Shape) {
        match shape {
            Shape::Vec(items) => {
                for item in items {
                    self.walk(item);
                }
            }
            Shape::Circle(circle) if circle.fill.a() > 0 => {
                self.dots.push((circle.center, circle.radius));
            }
            Shape::Rect(rect) => self.rects.push(rect.rect),
            Shape::Text(text) => self.labels.push((
                text.galley.text().to_string(),
                text.galley.rect.translate(text.pos.to_vec2()),
            )),
            _ => {}
        }
    }

    /// Where the string exactly equal to `label` was painted, if it was.
    fn box_of(&self, label: &str) -> Option<Rect> {
        self.labels
            .iter()
            .find(|(text, _)| text == label)
            .map(|(_, rect)| *rect)
    }

    /// The plate: the smallest wide, tall rectangle that has a dot on it. The card around it
    /// also holds the dots and is larger, and nothing else on the page is that wide with a
    /// place painted inside it.
    fn plate(&self) -> Rect {
        let holds_a_dot = |rect: &Rect| self.dots.iter().any(|(at, _)| rect.contains(*at));
        self.rects
            .iter()
            .copied()
            .filter(|rect| rect.width() > 600.0 && rect.height() > 120.0 && holds_a_dot(rect))
            .min_by(|left, right| {
                (left.width() * left.height())
                    .partial_cmp(&(right.width() * right.height()))
                    .expect("two finite areas")
            })
            .unwrap_or_else(|| {
                panic!(
                    "the page painted no plate with a place on it: {:?}",
                    self.rects
                )
            })
    }

    fn dots_on(&self, plate: Rect) -> Vec<(Pos2, f32)> {
        self.dots
            .iter()
            .copied()
            .filter(|(at, _)| plate.contains(*at))
            .collect()
    }
}

/// The page, driven one frame at a time.
struct Page {
    ctx: egui::Context,
    view: Overview,
    data: Snapshot,
    frame: usize,
}

impl Page {
    fn new(
        pins: Vec<Pin>,
        countries: Vec<(String, i64)>,
        articles: Vec<Article>,
        counts: Counts,
    ) -> Page {
        let ctx = egui::Context::default();
        atlas_desktop::theme::install(&ctx);
        Page {
            ctx,
            view: Overview::new(),
            data: Snapshot {
                pins,
                countries,
                articles,
                counts,
                hours: HOURS,
            },
            frame: 0,
        }
    }

    /// One frame with these events, and the answer the page gave.
    fn step(&mut self, events: Vec<egui::Event>) -> (Painted, Choice) {
        let mut choice = Choice::default();
        let time = self.frame as f64 / 60.0;
        self.frame += 1;
        let mut output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                choice = overview::show_choices(ui, &mut self.view, &self.data);
            },
        );
        let shapes = std::mem::take(&mut output.shapes);
        output.drop_without_applying_deltas();
        (Painted::record(&shapes), choice)
    }

    /// Frames until the plate stops easing in: the map frames its own camera on the first
    /// frame and spends a few frames reaching it, and an audit that clicked at a moving dot
    /// would be measuring the easing. It stops when the camera's zoom has held still for
    /// three frames.
    fn settle(&mut self) -> Painted {
        self.step(Vec::new());
        let mut still = 0;
        let mut last = self.camera().zoom();
        for _ in 0..120 {
            let painted = self.step(Vec::new()).0;
            let zoom = self.camera().zoom();
            if (zoom - last).abs() < 1e-6 {
                still += 1;
                if still >= 3 && !painted.dots_on(painted.plate()).is_empty() {
                    return painted;
                }
            } else {
                still = 0;
                last = zoom;
            }
        }
        panic!(
            "the plate never stopped easing: zoom {} after 120 frames",
            self.camera().zoom()
        );
    }

    fn camera(&self) -> Camera {
        self.view
            .map
            .as_ref()
            .expect("the page drew the map card")
            .camera()
    }

    /// What the plate's last press landed on: a dot, a merged dot, or a country. This is the
    /// view's whole answer, of which `show`'s `Option<usize>` is only the representative.
    fn picked(&self) -> Option<&Pick> {
        self.view
            .map
            .as_ref()
            .expect("the page drew the map card")
            .picked()
    }

    /// The dots the view's own clustering says it draws, in the plate's pixels.
    fn expected(&self, plate: Rect) -> Vec<Cluster> {
        let camera = self.camera();
        let centre = plate.center();
        let at = |pin: &Pin| {
            let point = camera.project(Geo::new(pin.lon as f32, pin.lat as f32));
            Pos2::new(centre.x + point.x, centre.y + point.y)
        };
        cluster(&self.data.pins, at, plate, CLUSTER_PX)
    }

    /// A press and a release at `at`: a click, as a pointer makes one.
    fn click(&mut self, at: Pos2) -> (Painted, Choice) {
        self.step(vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]);
        self.step(vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ])
    }
}

/// Pair every painted dot with the cluster it must be, so a click is aimed at a dot whose pin
/// is known rather than at a pixel that happens to hold one.
fn pair(painted: &Painted, expected: &[Cluster], plate: Rect) -> Vec<(Pos2, Cluster)> {
    let dots = painted.dots_on(plate);
    assert_eq!(
        dots.len(),
        expected.len(),
        "the plate painted {} dots, its own clustering says {}",
        dots.len(),
        expected.len()
    );
    let mut paired = Vec::with_capacity(dots.len());
    for (at, radius) in dots {
        let near = expected
            .iter()
            .min_by(|left, right| {
                left.at
                    .distance(at)
                    .partial_cmp(&right.at.distance(at))
                    .expect("two finite distances")
            })
            .expect("a dot to pair with");
        assert!(
            near.at.distance(at) < 0.5,
            "a dot painted at {at:?} is {} px from the nearest cluster, so this file cannot say \
             which pin it stands for",
            near.at.distance(at)
        );
        assert!(
            (near.radius - radius).abs() < 0.5,
            "the dot at {at:?} is {} px across, its cluster says {}",
            radius,
            near.radius
        );
        paired.push((at, near.clone()));
    }
    paired
}

/// The box of the chip a place is drawn as: `views::overview::chip_label` writes
/// `"{name} {count}"` and clips the name to sixteen characters (`views::clip`), so the chip is
/// matched by its *number* and by the name being a prefix of the place, rather than by a copy of
/// the clipping rule that could drift from it. The first match in paint order wins: a merged
/// dot's own chips are drawn before the country rollup's, and the two can share a word.
fn chip_of(painted: &Painted, place: &str, count: i64) -> Option<Rect> {
    let suffix = format!(" {}", nf(count));
    painted
        .labels
        .iter()
        .find(|(text, _)| {
            text.strip_suffix(&suffix).is_some_and(|name| {
                let name = name.trim_end_matches('…').trim_end();
                !name.is_empty() && place.starts_with(name)
            })
        })
        .map(|(_, rect)| *rect)
}

// ------------------------------------------------------- 1. the newest located stories

/// The newest 50 located stories each open, and each one is filed where its dot is.
///
/// For every row of the sample: the id resolves to exactly one article through
/// `Archive::articles_by_ids` (the read the window's top-up uses), with a title; the story's
/// label is a place the map draws, and the dot for that place is the **mean of that place's
/// coordinates** — recomputed here from the raw rows and asserted to 1e-4 — with the story's
/// own coordinate inside the group it was grouped into.
#[test]
fn the_newest_fifty_located_stories_open_and_sit_where_they_happened() {
    let Some((archive, conn)) = live() else {
        return;
    };
    let (pins, rows) = window_of(&archive, &conn);
    assert!(!rows.is_empty(), "the window has no located story to audit");
    assert!(!pins.is_empty(), "the window has rows but the map drew no pin");

    // The place each pin stands for, from the rows, so the centroid can be recomputed.
    let mut groups: BTreeMap<(String, String), Vec<&Row>> = BTreeMap::new();
    for row in &rows {
        groups
            .entry((row.label.clone(), row.iso.clone()))
            .or_default()
            .push(row);
    }

    let sample: Vec<&Row> = rows.iter().take(SAMPLE).collect();
    println!(
        "audit: {} located stories in the last {HOURS} h, {} places, {} pins; sample {}",
        rows.len(),
        groups.len(),
        pins.len(),
        sample.len()
    );

    let mut opened = 0;
    let mut widest = 0.0_f64;
    let mut unopened: Vec<i64> = Vec::new();
    for row in &sample {
        // (a) the id is a story the archive holds.
        let found = archive.articles_by_ids(&[row.id]);
        let one = found.iter().find(|article| article.id == row.id);
        match one {
            Some(article) if !article.title.trim().is_empty() => opened += 1,
            _ => unopened.push(row.id),
        }
        if let Some(article) = one {
            assert_eq!(
                article.published, row.published,
                "story {} reads at a different time than the row it was selected from",
                row.id
            );
        }

        // (b) the dot for its place exists and is the mean of that place's coordinates.
        let pin = pins
            .iter()
            .find(|pin| pin.place == row.label)
            .unwrap_or_else(|| {
                panic!(
                    "story {} is filed at {:?} and the map draws no dot for it: pins are {:?}",
                    row.id,
                    row.label,
                    pins.iter().map(|pin| pin.place.as_str()).collect::<Vec<_>>()
                )
            });
        let group = &groups[&(row.label.clone(), row.iso.clone())];
        let mean_lat = group.iter().map(|row| row.lat).sum::<f64>() / group.len() as f64;
        let mean_lon = group.iter().map(|row| row.lon).sum::<f64>() / group.len() as f64;
        assert!(
            (pin.lat - mean_lat).abs() < 1e-4 && (pin.lon - mean_lon).abs() < 1e-4,
            "the dot for {:?} sits at ({}, {}), the mean of its {} coordinates is ({}, {})",
            row.label,
            pin.lat,
            pin.lon,
            group.len(),
            mean_lat,
            mean_lon
        );
        widest = widest.max(
            ((row.lat - pin.lat).powi(2) + (row.lon - pin.lon).powi(2)).sqrt(),
        );

        // (c) and the story is one of the place's own.
        let at_place = archive.place_stories(&pin.place, HOURS, PINS);
        assert!(
            at_place.iter().any(|article| article.id == row.id),
            "story {} names {:?}, and the list for that place does not hold it",
            row.id,
            row.label
        );
    }

    println!(
        "audit: sample {} opened {opened}, {} could not be opened; widest story-to-dot distance \
         {widest:.3}°",
        sample.len(),
        unopened.len()
    );
    assert!(
        unopened.is_empty(),
        "{} of the newest {SAMPLE} located stories do not open: {unopened:?}",
        unopened.len()
    );

    // Every pin's destination resolves too — a dot that opens nothing is a door that does not
    // open, whatever the sample says.
    let destinations: Vec<i64> = pins.iter().map(|pin| pin.article).collect();
    let blind: Vec<&Pin> = pins.iter().filter(|pin| pin.article <= 0).collect();
    assert!(
        blind.is_empty(),
        "{} of {} dots carry no story ({}): a click on them opens nothing",
        blind.len(),
        pins.len(),
        blind
            .iter()
            .map(|pin| pin.place.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let resolved: BTreeSet<i64> = archive
        .articles_by_ids(&destinations)
        .into_iter()
        .map(|article| article.id)
        .collect();
    let missing: Vec<i64> = destinations
        .iter()
        .copied()
        .filter(|id| !resolved.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "{} dots open a story id the archive does not hold: {missing:?}",
        missing.len()
    );
    println!(
        "audit: {} dots, {} distinct stories behind them, all resolvable",
        pins.len(),
        resolved.len()
    );
}

// -------------------------------------------------- 2. every dot on the real plate opens

/// Every dot the shipped archive's plate paints, clicked, carried as a story id, and opened.
///
/// The chain is the page's own: painted dot → press and release → the plate's hit test →
/// `Pin::article` → `Choice.story`. A dot whose pin carries an id the archive does not hold
/// would be reported here, and every dot is clicked, so "every news marker must open its
/// article" is asserted for the whole map and not for a sample of it.
#[test]
fn every_dot_on_the_real_plate_opens_the_story_it_stands_for() {
    let Some((archive, conn)) = live() else {
        return;
    };
    let (pins, _rows) = window_of(&archive, &conn);
    let countries = archive.countries(HOURS, 14);
    let articles = archive.articles(HOURS, PINS, "");
    let counts = archive.counts();
    let mut page = Page::new(pins.clone(), countries, articles, counts);
    let painted = page.settle();
    let plate = painted.plate();
    let paired = pair(&painted, &page.expected(plate), plate);
    assert!(!paired.is_empty(), "the plate painted no dot to click");

    let mut opened = BTreeSet::new();
    let mut failures: Vec<String> = Vec::new();
    for (at, dot) in &paired {
        let pin = &pins[dot.rep];
        let want = pin.article;
        let (_painted, choice) = page.click(*at);
        match choice.story {
            Some(id) if id == want && want > 0 => {
                opened.insert(id);
            }
            other => failures.push(format!(
                "dot at {at:?} stands for {:?} ({} stories, id {want}), the click answered {other:?}",
                pin.place, pin.count
            )),
        }
    }

    println!(
        "audit: {} dots clicked, {} stories opened, {} unanswered",
        paired.len(),
        opened.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} dots did not open the story they stand for:\n{}",
        failures.len(),
        paired.len(),
        failures.join("\n")
    );

    // And every id a dot opened is a row: the page hands the window an id, not a row, and the
    // window tops its list up by key — an id the archive does not hold would open nothing.
    let ids: Vec<i64> = opened.iter().copied().collect();
    let held: BTreeSet<i64> = archive
        .articles_by_ids(&ids)
        .into_iter()
        .map(|article| article.id)
        .collect();
    let lost: Vec<i64> = ids.iter().copied().filter(|id| !held.contains(id)).collect();
    assert!(
        lost.is_empty(),
        "{} ids a click produced are not stories in the archive: {lost:?}",
        lost.len()
    );
}

// ---------------------------------------------------- 3. clustering never drops a marker

/// Every pin on the plate is in exactly one dot: the dots' `places` sum to the pins on the
/// plate and their `stories` sum to those pins' counts — at the fit, and at two zoom levels
/// where the dots split apart.
///
/// Also asserted: no two dots are closer than [`CLUSTER_PX`] (the plate's own promise that a
/// crowded coast never becomes a blob), every dot is inside the plate, and a dot that merged
/// more than one place keeps its heaviest pin as its representative — which is what a click
/// then opens.
#[test]
fn clustering_never_drops_a_marker() {
    let Some((archive, conn)) = live() else {
        return;
    };
    let (pins, _rows) = window_of(&archive, &conn);
    let countries = archive.countries(HOURS, 14);
    let articles = archive.articles(HOURS, PINS, "");
    let counts = archive.counts();
    let mut page = Page::new(pins.clone(), countries, articles, counts);
    let painted = page.settle();
    let plate = painted.plate();
    let camera = page.camera();

    let mut checked = 0;
    // The zoom is anchored on the heaviest pin, not on the plate's middle: the archive's
    // places are not spread evenly, and a zoom about the middle of the world leaves the plate
    // on an empty ocean (measured: 0 of 33 pins at ×4 with the middle held) — a pass with no
    // marker on it proves nothing about markers.
    let heaviest = pins
        .iter()
        .max_by_key(|pin| pin.count)
        .expect("a pin to zoom about");
    let anchor = camera.project(Geo::new(heaviest.lon as f32, heaviest.lat as f32));
    for factor in [1.0_f32, 2.0, 4.0] {
        let mut zoomed = camera;
        zoomed.zoom_at(anchor, factor);
        let centre = plate.center();
        let at = |pin: &Pin| {
            let point = zoomed.project(Geo::new(pin.lon as f32, pin.lat as f32));
            Pos2::new(centre.x + point.x, centre.y + point.y)
        };
        let dots = cluster(&pins, at, plate, CLUSTER_PX);
        let inside: Vec<&Pin> = pins
            .iter()
            .filter(|pin| plate.contains(at(pin)))
            .collect();
        let places: usize = dots.iter().map(|dot| dot.places).sum();
        let stories: i64 = dots.iter().map(|dot| dot.stories).sum();
        let counted: i64 = inside.iter().map(|pin| pin.count).sum();
        println!(
            "clustering at ×{factor}: {} pins on the plate → {} dots ({} places, {} stories), \
             {} stories counted",
            inside.len(),
            dots.len(),
            places,
            stories,
            counted
        );
        assert_eq!(
            places,
            inside.len(),
            "at ×{factor} the dots claim {places} places and {} pins are on the plate: {} \
             markers were dropped or counted twice",
            inside.len(),
            inside.len() as i64 - places as i64
        );
        assert_eq!(
            stories, counted,
            "at ×{factor} the dots claim {stories} stories and the pins on the plate carry \
             {counted}"
        );

        let mut tightest = f32::INFINITY;
        for (index, dot) in dots.iter().enumerate() {
            assert!(
                plate.contains(dot.at),
                "a dot was drawn at {:?}, outside the plate {plate:?}",
                dot.at
            );
            // A dot stands for at least its own representative, and claims no more stories
            // than its places carry.
            let rep = &pins[dot.rep];
            assert!(
                dot.rep < pins.len() && dot.stories >= rep.count && dot.places >= 1,
                "the dot at {:?} claims {} places and {} stories but stands for pin {} with {} \
                 stories",
                dot.at,
                dot.places,
                dot.stories,
                dot.rep,
                rep.count
            );
            for other in dots.iter().skip(index + 1) {
                tightest = tightest.min(dot.at.distance(other.at));
            }
        }
        if dots.len() > 1 {
            assert!(
                tightest >= CLUSTER_PX,
                "at ×{factor} two dots are {tightest:.1} px apart, under the {CLUSTER_PX} px the \
                 clustering promises"
            );
        }
        checked += dots.len();
    }
    assert!(checked > 0, "the audit clustered nothing");
}

// -------------------------------------------------- 4. a list that opens its own articles

/// The page's list door: a chip says how many stories a place has, so the list it opens must
/// hold them and every one of them must resolve to an article. This is the shape a cluster's
/// list has to have, and it is asserted here so the finding below is about the cluster and not
/// about the page's ability to draw a list at all.
///
/// **What it found.** The chips under the plate are the *country* rollup (`Archive::countries`,
/// keyed by `iso`), but the page reports the chip as a **place** and the window opens
/// `Archive::place_stories` with it — which matches the place *label*
/// (`coalesce(nullif(place, ''), nullif(iso, ''), '')`). A country whose stories all name a
/// city has no row under the label `USA`, so the chip says "USA 79" and the list it opens is
/// empty. `Archive::country_stories` is the read that matches the count.
#[test]
fn the_place_list_a_click_produces_opens_its_articles() {
    let Some((archive, conn)) = live() else {
        return;
    };
    let (pins, _rows) = window_of(&archive, &conn);
    let countries = archive.countries(HOURS, 14);
    assert!(!countries.is_empty(), "the rollup is empty in this window");
    let articles = archive.articles(HOURS, PINS, "");
    let counts = archive.counts();
    let mut page = Page::new(pins, countries.clone(), articles, counts);
    page.settle();

    // The chip is drawn as its own text (`"{iso} {count}"`, `overview.rs::chip_label`), so the
    // click is aimed at the box the page painted that string in — not at a guessed pixel.
    let (iso, count) = countries[0].clone();
    let label = format!("{iso} {}", nf(count));
    let (painted, _) = page.step(Vec::new());
    let box_ = painted.box_of(&label).unwrap_or_else(|| {
        panic!(
            "the page painted no chip labelled {label:?}; it painted {:?}",
            painted.labels
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
        )
    });
    let (_painted, choice) = page.click(box_.center());
    let door = choice.door().unwrap_or_else(|| {
        panic!("a click on the {iso} chip ({count} stories) answered no door at all")
    });
    assert_eq!(
        door.word, iso,
        "a click on the {iso} chip ({count} stories) answered the door {door:?}"
    );
    // The chip is a *country* rollup, so the door has to say country. This is the half of the
    // defect that used to be invisible: the answer carried a bare `String`, so the window could
    // not tell a country from a place and read the wrong column.
    assert_eq!(
        door.kind,
        Kind::Country,
        "the {iso} chip is keyed by `iso` (`Archive::countries`) but its door says {:?}",
        door.kind
    );

    // What the window opens for that answer, and what the chip's number counts. The read goes
    // through the product's own hand-off (`app::stories_for`), not a copy of it here: the
    // promise under audit is that the chip's number and the chip's list are the same rows.
    let list = atlas_desktop::app::stories_for(&archive, &door, HOURS, atlas_desktop::app::LIST_ROWS);
    let by_country = archive.country_stories(&iso, HOURS, PINS);
    let by_place = archive.place_stories(&iso, HOURS, PINS);
    println!(
        "audit: the {iso} chip claims {count} stories; the door {:?} opens {} \
         (`country_stories` holds {}, `place_stories` holds {})",
        door.kind,
        list.len(),
        by_country.len(),
        by_place.len()
    );
    assert_eq!(
        list.len() as i64,
        count.min(atlas_desktop::app::LIST_ROWS),
        "the {iso} chip says {count} stories and the list its click opens holds {} \
         (`country_stories` holds {}, the place *label* read holds {})",
        list.len(),
        by_country.len(),
        by_place.len()
    );
    let ids: Vec<i64> = list.iter().map(|article| article.id).collect();
    let held: BTreeSet<i64> = archive
        .articles_by_ids(&ids)
        .into_iter()
        .map(|article| article.id)
        .collect();
    let lost: Vec<i64> = ids.iter().copied().filter(|id| !held.contains(id)).collect();
    assert!(
        lost.is_empty(),
        "{} stories in the {iso} list do not open: {lost:?}",
        lost.len()
    );
}

// ------------------------------------------------------- 5. a cluster opens its places

/// **The finding, and what it now is.** This test was written against a `Cluster` that carried
/// `rep` and counts only: the busiest merged dot of the shipped archive stood for 3 places and
/// 99 stories, and a click answered one id — the other 98 were unreachable from the map. It
/// fails to this day in the form it was written, so it is restated here as the promise the
/// fixed page makes, and the name is kept because `views/map.rs` cites it.
///
/// The bound: a press on a merged dot must answer the **whole** dot. Its own press opens the
/// representative's story (a dot is still a door onto one reading), `MapView::picked` must name
/// every pin under it, the card must draw each of those places as its own chip, and every chip's
/// door must open that place's stories — the list the dot's stories are in.
#[test]
fn a_dot_for_several_places_opens_only_one_of_them() {
    let Some((archive, conn)) = live() else {
        return;
    };
    let (pins, _rows) = window_of(&archive, &conn);
    let countries = archive.countries(HOURS, 14);
    let articles = archive.articles(HOURS, PINS, "");
    let counts = archive.counts();
    let mut page = Page::new(pins.clone(), countries, articles, counts);
    let painted = page.settle();
    let plate = painted.plate();
    let paired = pair(&painted, &page.expected(plate), plate);

    let merged: Vec<(Pos2, Cluster)> = paired
        .iter()
        .filter(|(_, dot)| dot.places > 1)
        .cloned()
        .collect();
    println!(
        "audit: {} of {} dots stand for more than one place",
        merged.len(),
        paired.len()
    );
    if merged.is_empty() {
        println!("audit: the shipped archive merges no places at this size, so nothing to audit");
        return;
    }

    let busiest = merged
        .iter()
        .max_by_key(|(_, dot)| dot.stories)
        .expect("a merged dot");
    let (at, dot) = busiest.clone();
    let (painted, choice) = page.click(at);

    // (a) The dot still opens one story — the newest at its heaviest place.
    let story = choice
        .story
        .unwrap_or_else(|| panic!("a press on the {}-place dot at {at:?} opened nothing", dot.places));
    assert_eq!(
        story, pins[dot.rep].article,
        "the dot's press opened {story}, its representative {:?} carries {}",
        pins[dot.rep].place, pins[dot.rep].article
    );

    // (b) And it names every place under it.
    let picked = page.picked();
    let members = match picked {
        Some(Pick::Places(members)) => members.clone(),
        other => panic!(
            "the {}-place dot at {at:?} answered the pick {other:?}: a dot that stands for \
             several places has to name them",
            dot.places
        ),
    };
    println!(
        "audit: the busiest dot names {} places ({} stories): {:?}",
        members.len(),
        dot.stories,
        members
            .iter()
            .map(|slot| format!("{} {}", pins[*slot].place, pins[*slot].count))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        members.len(),
        dot.places,
        "the dot claims {} places and names {}",
        dot.places,
        members.len()
    );
    assert_eq!(
        members, dot.members,
        "the pick's places are not the dot's own members"
    );

    // (c) The card draws each of them, and each chip opens that place's stories.
    let mut reachable: BTreeSet<i64> = BTreeSet::new();
    for slot in &members {
        let pin = &pins[*slot];
        let corner = chip_of(&painted, &pin.place, pin.count).unwrap_or_else(|| {
            panic!(
                "the card drew no chip for {:?} ({} stories), one of the {} places of the dot at \
                 {at:?}; it drew {:?}",
                pin.place,
                pin.count,
                dot.places,
                painted
                    .labels
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<Vec<_>>()
            )
        });
        let (_frame, chip) = page.click(corner.center());
        let door = chip.door().unwrap_or_else(|| {
            panic!(
                "a press on the {:?} chip under the dot answered no door",
                pin.place
            )
        });
        assert_eq!(
            door.kind,
            Kind::Place,
            "the chip for {:?} under a merged dot reports {:?}: a dot's members are place \
             labels, not countries",
            pin.place,
            door.kind
        );
        assert_eq!(door.word, pin.place);
        let list = stories_for(&archive, &door, HOURS, LIST_ROWS);
        assert!(
            !list.is_empty(),
            "the {:?} chip under the dot opens an empty list",
            pin.place
        );
        assert!(
            list.iter().any(|article| article.id == pin.article),
            "the list for {:?} does not hold the story the dot's press opens ({})",
            pin.place,
            pin.article
        );
        reachable.extend(list.iter().map(|article| article.id));
    }
    assert!(
        reachable.len() as i64 >= dot.stories,
        "the {} places of the dot at {at:?} open {} stories between them, and the dot claims {}",
        members.len(),
        reachable.len(),
        dot.stories
    );
}

// ------------------------------------------------------ 6. the whole promise, end to end

/// **Every dot is a door that opens, and the dots between them reach the whole window.**
///
/// On the shipped archive, for every dot the plate paints:
///
/// * a press on it opens at least one article — the id the page answers is a row of the archive
///   (`Archive::articles_by_ids`), not an id this page invented;
/// * every place the dot stands for opens a non-empty list, read through the product's own
///   hand-off (`app::stories_for`), and the dot's own story is in its own place's list;
/// * and the union of all of it equals the located stories of the window: **nothing
///   unreachable** (every located story is behind some dot) and **nothing invented** (every id
///   a door opens is a row of the window, `relevant` and not a duplicate).
///
/// The equality is stated as two halves and not as one `==` because `place_stories` matches the
/// place *label* and does not ask for a coordinate: a story filed under a place but without a
/// position is in the same list, and that is the list, not an invention. The count of such rows
/// is printed; on this archive it is zero and the two sets are equal.
#[test]
fn every_dot_destination_opens_and_the_dots_between_them_cover_the_window() {
    let Some((archive, conn)) = live() else {
        return;
    };
    let (pins, rows) = window_of(&archive, &conn);
    let countries = archive.countries(HOURS, 14);
    let articles = archive.articles(HOURS, PINS, "");
    let counts = archive.counts();
    let mut page = Page::new(pins.clone(), countries, articles, counts);
    let painted = page.settle();
    let plate = painted.plate();
    let paired = pair(&painted, &page.expected(plate), plate);

    let mut reachable: BTreeSet<i64> = BTreeSet::new();
    let mut silent: Vec<String> = Vec::new();
    for (at, dot) in &paired {
        let (_frame, choice) = page.click(*at);
        // (a) the press opens at least one article, and that article exists.
        match choice.story {
            Some(id) if archive.articles_by_ids(&[id]).len() == 1 => {
                reachable.insert(id);
            }
            other => silent.push(format!(
                "the dot at {at:?} ({} places, {} stories) opened {other:?}",
                dot.places, dot.stories
            )),
        }
        // (b) every place the dot stands for opens its list, and holds the dot's own story.
        for slot in &dot.members {
            let pin = &pins[*slot];
            let door = Door::place(&pin.place, pin.count);
            let list = stories_for(&archive, &door, HOURS, LIST_ROWS);
            if list.is_empty() {
                silent.push(format!(
                    "the place {:?} behind the dot at {at:?} opens an empty list",
                    pin.place
                ));
                continue;
            }
            if !list.iter().any(|article| article.id == pin.article) {
                silent.push(format!(
                    "the list for {:?} does not hold the story the dot opens ({})",
                    pin.place, pin.article
                ));
            }
            reachable.extend(list.iter().map(|article| article.id));
        }
    }
    assert!(
        silent.is_empty(),
        "{} doors on the plate open nothing they promise:\n{}",
        silent.len(),
        silent.join("\n")
    );

    let located: BTreeSet<i64> = rows.iter().map(|row| row.id).collect();
    let unreachable: Vec<i64> = located.difference(&reachable).copied().collect();
    let beyond: Vec<i64> = reachable.difference(&located).copied().collect();
    println!(
        "audit: {} dots reach {} articles; the window has {} located stories; {} unreachable, \
         {} reachable without a coordinate",
        paired.len(),
        reachable.len(),
        located.len(),
        unreachable.len(),
        beyond.len()
    );
    assert!(
        unreachable.is_empty(),
        "{} of the {} located stories are behind no door on the plate: {unreachable:?}",
        unreachable.len(),
        located.len()
    );

    // Nothing invented: every id a door opened is a row of this window, relevant and not a
    // duplicate, even when it carries no coordinate.
    let mut statement = conn
        .prepare(
            "SELECT coalesce(iso, '') FROM articles \
             WHERE id = ?1 AND relevant = 1 AND dupe = 0 AND published >= ?2",
        )
        .expect("the window's own check");
    for id in &beyond {
        let held: usize = statement
            .query_map(rusqlite::params![id, window_bound(&conn)], |row| {
                row.get::<_, String>(0)
            })
            .expect("the check runs")
            .flatten()
            .count();
        assert_eq!(
            held, 1,
            "a door opened {id}, which is not a relevant, first-seen row of this window: the \
             page invented a story"
        );
    }
    assert_eq!(
        beyond.len(),
        0,
        "the doors reach {} stories that carry no coordinate of their own: they are filed under \
         a place the map draws, so they are in that place's list",
        beyond.len()
    );
}
