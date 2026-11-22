//! A window with no window: three frames of every page, drawn from a temporary archive.
//!
//! This is the one place the desktop app's drawing code runs without a GUI session. Two
//! facts make it possible and both are measured, not assumed:
//!
//! * **The page code is ordinary `egui` code.** A page is a function that takes a
//!   `&mut egui::Ui`; `egui::Context::run` lays out, paints and returns the shapes
//!   without a window, a GPU surface or a driver. `RawInput::screen_rect` is the whole
//!   of what a frame needs to know about a screen.
//! * **The app's own shell is reachable this way too.** `eframe 0.31.1` exposes
//!   `eframe::Frame::_new_kittest()` and `eframe::CreationContext::_new_kittest(ctx)`,
//!   so neither a window nor a `Frame` is required; and `app::Atlas` gained the three
//!   hooks a test needs: `Atlas::with_archive(path)` (so the window opens *this* test's
//!   temporary archive instead of `crate::assets::archive()`), `Atlas::draw(ui)` (the
//!   whole frame — rail, status row and page — with no `eframe::Frame`), and
//!   `set_page`/`page` over a now-public `app::Page`. `eframe::App::update` is one call to
//!   `draw`, so what is exercised below is what ships.
//!
//! Two levels of test, on purpose. [`each_page`] measures one page's drawing with the
//! panel built here, which is how the pages stay comparable to each other and to their
//! empty states; [`the_real_window_draws_its_rail_status_row_and_every_page`] drives one
//! real `Atlas` instead, which is the only way to cover the rail, the status row, the
//! once-per-demand read decision and the switch between pages.
//!
//! # What is imported
//!
//! The pages, the theme and the queries come from the crate's own `[lib]` target
//! (`atlas_desktop`), so this test links *the same compiled code* the window runs: the
//! same `data::Article`, the same `views::graph::GraphView`, the same palette. Nothing
//! is copied into `tests/`, and the crate's unit tests run once, in the library's own
//! test target, rather than a second time from here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::app::{Atlas, Page, PAGES};
use atlas_desktop::data::{self, Archive};
use atlas_desktop::http;
use atlas_desktop::theme;
use atlas_desktop::views;
use atlas_desktop::views::articles::ArticlesView;
use atlas_desktop::views::graph::GraphView;
use atlas_desktop::views::overview::{Overview, Snapshot};

/// The screen every frame is told about. The app's own default window size
/// (`src/main.rs:33-35`) so the pages lay out as they do on a real 1440×900 screen.
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

/// How many frames each page gets. Three, because the graph settles over frames
/// (`GraphView::show` ticks its layout on every call) and a page that only survives
/// its first frame — a widget whose id collides on the second pass, a scroll area
/// that only breaks once it has a height — is exactly the bug this is for.
const FRAMES: usize = 3;

/// The window the pages under test were built from (`app.rs::reload`'s window).
const HOURS: i64 = 72;

// ---------------------------------------------------------------------------
// a temporary archive, and the window's state read out of it
// ---------------------------------------------------------------------------

/// A `data/atlas.db` of our own, in a temp directory, removed when the test ends.
///
/// The live archive is never opened: a test that read the user's reading history
/// would be neither hermetic nor repeatable, and every count below would change
/// under it. The schema is the subset `data.rs` queries (one `CREATE TABLE` per
/// query in that file); `Archive::open` then reads it through the same
/// `SQLITE_OPEN_READ_ONLY` connection the window uses.
struct TempArchive {
    dir: PathBuf,
    file: PathBuf,
}

/// Distinguishes archives built in the same nanosecond on different test threads.
///
/// The suite runs in parallel, the clock's resolution is coarser than a thread switch,
/// and two archives that landed on one directory made `CREATE TABLE` fail with "table
/// articles already exists" — measured, once, on a parallel run.
static NEXT_ARCHIVE: AtomicU64 = AtomicU64::new(0);

impl TempArchive {
    fn build() -> TempArchive {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-desktop-smoke-{}-{}-{}",
            std::process::id(),
            nanos,
            NEXT_ARCHIVE.fetch_add(1, Ordering::Relaxed)
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

        let recent = stamp(2);
        let older = stamp(30);
        conn.execute(
            "INSERT INTO sources(id, name) VALUES(1,'The Nordic Post'),(2,'Alpine Review')",
            [],
        )
        .expect("two sources");
        // Six stories, six countries, six distinct coordinates: enough that the map
        // has markers to place and the article list has rows to click. `place` and
        // `iso` are both filled because `pins()` labels a marker with whichever exists.
        let stories: [(i64, &str, i64, &str, &str, &str, f64, f64); 6] = [
            (1, "Harbour crane strike ends after nine days", 1, "DEU", "Hamburg", recent.as_str(), 54.0, 10.0),
            (2, "Alpine rail freight reaches a record", 2, "AUT", "Vienna", recent.as_str(), 48.2, 16.4),
            (3, "Chip export rules tighten in Asia", 1, "JPN", "Tokyo", older.as_str(), 35.7, 139.7),
            (4, "Drought reshapes the southern harvest", 2, "ESP", "Madrid", recent.as_str(), 40.4, -3.7),
            (5, "A second harbour approves the contract", 1, "NLD", "Rotterdam", recent.as_str(), 51.9, 4.5),
            (6, "Ferry traffic resumes on the northern route", 2, "NOR", "Oslo", older.as_str(), 59.9, 10.7),
        ];
        for (id, title, source, iso, place, published, lat, lon) in stories {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe, lat, lon) \
                 VALUES(?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 1, 0, ?10, ?11)",
                rusqlite::params![
                    id,
                    title,
                    source,
                    published,
                    format!("{title} — a summary sentence for the list."),
                    format!("{title}. The body of the story, long enough to fill the reading pane."),
                    iso,
                    place,
                    format!("https://example.invalid/{id}"),
                    lat,
                    lon
                ],
            )
            .expect("a story");
        }
        conn.execute(
            "INSERT INTO article_links(a, b) VALUES(1,2),(2,3),(3,4),(4,5),(5,6)",
            [],
        )
        .expect("links");
        conn.execute("INSERT INTO alerts(id) VALUES(1),(2),(3)", [])
            .expect("alerts");
        for (id, name, kind, count) in [
            (1, "Alpha Group", "ORG", 40),
            (2, "North Harbour", "LOC", 31),
            (3, "Beta Union", "ORG", 22),
            (4, "C. Reporter", "PER", 18),
            (5, "Alpine Rail", "ORG", 14),
            (6, "Southern Growers", "ORG", 11),
            (7, "Ferry Line", "ORG", 9),
            (8, "Ministry of Trade", "ORG", 8),
        ] {
            conn.execute(
                "INSERT INTO entities(id, name, type, count, last_seen) VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, name, kind, count, recent],
            )
            .expect("an entity");
        }
        for (a, b, weight, label) in [
            (1, 2, 6.0, "Alpha Group and North Harbour share 3 stories"),
            (1, 3, 4.5, "Alpha Group and Beta Union share 2 stories"),
            (2, 5, 3.0, "North Harbour and Alpine Rail share a story"),
            (3, 4, 2.5, "Beta Union and C. Reporter share a story"),
            (5, 8, 2.0, "Alpine Rail and Ministry of Trade share a story"),
            (6, 7, 1.5, "Southern Growers and Ferry Line share a story"),
            (1, 6, 1.2, "Alpha Group and Southern Growers share a story"),
        ] {
            conn.execute(
                "INSERT INTO edges(a, b, weight, label) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![a, b, weight, label],
            )
            .expect("a relation");
        }
        for (id, name, keywords, active, hits) in [
            (1, "Chip supply", "chip, semiconductor, export", 1, 12),
            (2, "Harbour labour", "harbour, strike, dock", 1, 7),
            (3, "Alpine freight", "rail, freight, alpine", 0, 3),
        ] {
            conn.execute(
                "INSERT INTO topics(id, name, keywords, active, hits, last_hit) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, name, keywords, active, hits, recent],
            )
            .expect("a topic");
        }
        for (id, name, query, active, cadence, hits) in [
            (1, "Chip export rules", "chip export restrictions", 1, 60, 4),
            (2, "North Sea ferries", "ferry north sea traffic", 0, 240, 1),
        ] {
            conn.execute(
                "INSERT INTO monitors(id, name, query, active, cadence_minutes, last_run, hits) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![id, name, query, active, cadence, recent, hits],
            )
            .expect("a monitor");
        }
        drop(conn);
        TempArchive { dir, file }
    }

    fn open(&self) -> Archive {
        Archive::open(&self.file).expect("the temporary archive opens read-only")
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The window: everything `app.rs::reload` reads, read the way it reads it.
struct Window {
    temp: TempArchive,
    counts: data::Counts,
    pins: Vec<data::Pin>,
    countries: Vec<(String, i64)>,
    articles: Vec<data::Article>,
    entities: Vec<data::Entity>,
    edges: Vec<data::Edge>,
    topics: Vec<data::Topic>,
    monitors: Vec<data::Monitor>,
}

impl Window {
    /// `app.rs::reload` (from `src/app.rs:250`), call for call: one read-only
    /// connection, one query per list.
    fn open() -> Window {
        let temp = TempArchive::build();
        let archive = temp.open();
        let counts = archive.counts();
        let pins = archive.pins(HOURS, 400);
        let countries = archive.countries(HOURS, 14);
        let articles = archive.articles(HOURS, 400, "");
        let entities = archive.entities(HOURS, 120);
        let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
        let edges = archive.edges(&ids, 900);
        let topics = archive.topics();
        let monitors = archive.monitors();
        Window {
            temp,
            counts,
            pins,
            countries,
            articles,
            entities,
            edges,
            topics,
            monitors,
        }
    }

    fn path(&self) -> &Path {
        &self.temp.file
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            pins: self.pins.clone(),
            countries: self.countries.clone(),
            articles: self.articles.clone(),
            counts: self.counts,
            hours: HOURS,
        }
    }

    /// The same window with nothing in it: what a page paints when the archive is
    /// empty, which is how the empty-state sentences are discovered rather than
    /// guessed (see [`each_page_names_its_empty_state_and_a_full_one_paints_none`]).
    fn empty(&self) -> Snapshot {
        Snapshot {
            pins: Vec::new(),
            countries: Vec::new(),
            articles: Vec::new(),
            counts: data::Counts::default(),
            hours: HOURS,
        }
    }
}

/// `now - hours` as the archive's own stamp (`YYYY-MM-DDTHH:MM:SSZ`), so
/// `published >= since_iso(hours)` holds for the seeded rows. Howard Hinnant's
/// algorithm again, because the crate has no time dependency and neither does this.
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
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

// ---------------------------------------------------------------------------
// one frame, and what it painted
// ---------------------------------------------------------------------------

/// What one page painted over its frames.
#[derive(Default)]
struct Painted {
    /// Drawn shapes, text excluded: lines, paths, circles, rects, meshes.
    shapes: usize,
    /// Circles alone, which is what the graph's nodes and the map's markers are.
    circles: usize,
    /// Every string that reached a galley, one per line.
    text: String,
    /// Where each string landed. The rail's page labels are painted text inside
    /// `selectable_label`s, so a rectangle is enough to aim a synthetic click at the very
    /// widget a reader would press — no remembered coordinates.
    labels: Vec<(String, Rect)>,
}

impl Painted {
    fn text(&self) -> &str {
        &self.text
    }

    /// True when `needle` was painted — the positive half of a smoke assertion.
    fn paints(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    /// How many separate strings reached a galley.
    fn text_count(&self) -> usize {
        self.text.lines().count()
    }

    /// Where the string exactly equal to `needle` was painted — the galley's own
    /// rectangle — or `None`.
    ///
    /// Exact, not a substring: "Graph" is the rail's label, while "6 entities and 7
    /// relations in the window — click a node" is the page's own line, and a click has to
    /// land on the first.
    fn rect_of(&self, needle: &str) -> Option<Rect> {
        self.labels
            .iter()
            .find(|(text, _)| text == needle)
            .map(|(_, rect)| *rect)
    }
}

/// A context with the app's own style installed, as `Atlas::new` does
/// (`src/app.rs:201`), so the pages are measured with the fonts, spacing and palette
/// they ship with.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// The input of one frame: a screen, no pointer, no keys, a clock that moves.
fn frame_input(index: usize) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(index as f64 * (1.0 / 60.0)),
        ..Default::default()
    }
}

/// One frame, with the frame's `TexturesDelta` released before it is dropped.
///
/// `epaint 0.37` panics on `Drop` when a `TexturesDelta` still carries unapplied deltas
/// (`epaint-0.37.0/src/textures.rs:337`, "Deltas need to be handled"). A real backend
/// uploads them; there is no renderer here, and the assertions below read `shapes`, which
/// are complete without a single texture uploaded. So the deltas are cleared explicitly —
/// the handling that crate asks for — instead of arriving at `Drop` and panicking, which
/// would make every page fail for a reason that has nothing to do with the page.
fn frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    mut draw: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, |ui| draw(ui));
    output.textures_delta.clear();
    output
}

/// Run `frames` frames of `page` and collect what came out.
///
/// The page is drawn inside the same containers `app.rs` puts it in — a
/// `CentralPanel` with the page's inner margin and a vertical `ScrollArea` that never
/// shrinks — so a page that only breaks inside a scroll area still breaks here.
fn paint(ctx: &egui::Context, frames: usize, mut page: impl FnMut(&mut egui::Ui)) -> Painted {
    let mut painted = Painted::default();
    for index in 0..frames {
        // `egui 0.37` hands the frame a root `Ui` (`Context::run_ui`), and a panel is
        // shown inside it — the same shape `app::Atlas::draw` uses.
        let output = frame(ctx, frame_input(index), |ui| {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::PAGE)
                        .inner_margin(egui::Margin::symmetric(20, 16)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| page(ui));
                });
        });
        // The last frame is the one asserted on: an animation (the graph's cooling)
        // has had two frames to settle, and the text is the same in all three.
        painted = measure(&output.shapes);
    }
    painted
}

/// Walk the frame's shapes: count what was drawn and keep every galley's text.
fn measure(shapes: &[egui::epaint::ClippedShape]) -> Painted {
    let mut painted = Painted::default();
    for clipped in shapes {
        walk(&clipped.shape, &mut painted);
    }
    painted
}

fn walk(shape: &Shape, painted: &mut Painted) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, painted);
            }
        }
        Shape::Text(text) => {
            let string = text.galley.text().to_string();
            painted
                .labels
                .push((string.clone(), text.galley.rect.translate(text.pos.to_vec2())));
            painted.text.push_str(&string);
            painted.text.push('\n');
        }
        Shape::Circle(_) => {
            painted.circles += 1;
            painted.shapes += 1;
        }
        _ => painted.shapes += 1,
    }
}

/// Run `frames` frames of the **real window** — rail, status row and page alike — and
/// collect what came out.
///
/// `Atlas::draw` takes only a `&mut egui::Ui` (`src/app.rs`) — egui 0.37's root ui, which
/// `Context::run_ui` builds — which is the whole reason this file can draw the shipped
/// window with no window: the `eframe::Frame` that `eframe::App::ui` also receives is
/// never read by the app.
fn paint_window(ctx: &egui::Context, frames: usize, atlas: &mut Atlas) -> Painted {
    let mut painted = Painted::default();
    for index in 0..frames {
        let output = frame(ctx, frame_input(index), |ui| atlas.draw(ui));
        painted = measure(&output.shapes);
    }
    painted
}

/// Press and release the primary button at `at`: what a reader clicking a rail label does.
///
/// Two frames, because `egui` reports `Response::clicked()` on the *release* at the point
/// the press happened. The pointer is moved first, so the widget under the point is the
/// one that gets the press.
fn click(ctx: &egui::Context, atlas: &mut Atlas, at: Pos2) -> Painted {
    let input = |pressed: bool| egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
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
    frame(ctx, input(true), |ui| atlas.draw(ui));
    let output = frame(ctx, input(false), |ui| atlas.draw(ui));
    measure(&output.shapes)
}

// ---------------------------------------------------------------------------
// the five pages
// ---------------------------------------------------------------------------

/// The five pages, drawn exactly as `app.rs::update`'s `match self.page` draws them.
///
/// The closures own the page's own state (`Overview`, `ArticlesView`, `GraphView`), so
/// a frame that mutates it — the graph's tick, the article pane's selection — is
/// carried into the next frame, which is what the window does.
fn each_page(
    window: &Window,
    mut draw: impl FnMut(&'static str, &mut dyn FnMut(&mut egui::Ui)),
) {
    let snapshot = window.snapshot();

    // Overview: the map, then the stories of the window.
    let mut overview = Overview::new();
    draw("Overview", &mut |ui| {
        if let Some(id) = views::overview::show(ui, &mut overview, &snapshot) {
            assert!(id > 0, "the overview answered an impossible story id {id}");
        }
    });

    // Articles: the list on the left, the reading pane on the right. The first story
    // is selected so the reading pane draws its real content and not its prompt.
    let mut articles = ArticlesView::default();
    articles.selected = window.articles.first().map(|article| article.id);
    draw("Articles", &mut |ui| {
        views::articles::show(ui, &mut articles, &window.articles, HOURS);
    });

    // Reader: the story's own page. A headless test has no engine, so this is the shape the
    // shell draws on a build without one — the same `show` call with `None` where the engine
    // would be, and the archive's copy of the words under the sentence that says so.
    let mut reader_story = window.articles.first().map(views::reader::Story::of);
    draw("Reader", &mut |ui| {
        // The heading this harness supplies, the way the Graph block above supplies its own:
        // the shell's Reader page has no title of its own (the page's own words are the
        // title), so a page that says nothing at all would be indistinguishable from a page
        // that failed to draw.
        ui.label(theme::page_title("Reader"));
        ui.add_space(10.0);
        match reader_story.as_mut() {
            Some(story) => {
                views::reader::show(ui, story, None, false);
            }
            None => {
                ui.label(theme::dim("No page is open."));
            }
        }
    });

    // Graph: the layout plate and the relation panel, in the two columns
    // `app.rs::update` (from `src/app.rs:400`) allocates them in.
    let mut graph = GraphView::default();
    graph.fill(window.entities.clone(), window.edges.clone());
    draw("Graph", &mut |ui| {
        ui.label(theme::page_title("Graph"));
        ui.add_space(10.0);
        ui.horizontal_top(|ui| {
            let width = (ui.available_width() - views::graph::PANEL_WIDTH - 16.0).max(320.0);
            ui.allocate_ui(egui::vec2(width, 620.0), |ui| {
                graph.show(ui, 600.0);
            });
            ui.allocate_ui(egui::vec2(views::graph::PANEL_WIDTH, 620.0), |ui| {
                views::graph::panel(ui, &graph);
            });
        });
    });

    draw("Topics", &mut |ui| {
        views::topics::show(ui, &window.topics);
    });
    draw("Monitors", &mut |ui| {
        views::monitors::show(ui, &window.monitors);
    });

    // A sixth page landed (`views::chat`) while this file was being written. The
    // five above are the pages this smoke test was asked for; this one is drawn too,
    // because a page that nothing draws is the page that breaks first.
    let mut chat = views::chat::ChatState::default();
    draw("Chat", &mut |ui| {
        views::chat::show(ui, &mut chat, "", http::Budget::SHIPPED);
    });
}

// ---------------------------------------------------------------------------
// the test
// ---------------------------------------------------------------------------

/// Every page draws three frames with no window, no GPU and no GUI session.
///
/// The archive is a temporary one built by this test, so the numbers a page paints are
/// the numbers of the six stories below and nothing on this machine can change them.
#[test]
fn the_five_pages_paint_three_frames_with_no_window() {
    let window = Window::open();
    // The window is real: the pages are about to draw data, not an empty state.
    // A query that fails to prepare answers an empty list (`data.rs` swallows the
    // error), so every list is asserted non-empty: a schema in this file that has
    // fallen out of step with `data.rs` then fails here with a name, instead of
    // painting five empty pages and passing.
    let expect = |what: &str, got: usize, want: usize| {
        assert_eq!(
            got, want,
            "{what}: {got} — a `data.rs` query that does not prepare returns nothing, \
             so check this file's schema against it"
        );
    };
    expect("stories in the window", window.articles.len(), 6);
    expect("the status rail's story count", window.counts.relevant as usize, 6);
    expect("marked places", window.pins.len(), 6);
    expect("countries", window.countries.len(), 6);
    expect("entities in the window", window.entities.len(), 8);
    expect("relations in the window", window.edges.len(), 7);
    expect("topics", window.topics.len(), 3);
    expect("monitors", window.monitors.len(), 2);

    let ctx = context();
    let mut seen: Vec<(&str, Painted)> = Vec::new();
    each_page(&window, |name, page| {
        let painted = paint(&ctx, FRAMES, page);
        // A page with data paints its own name and its own numbers; a page that
        // silently fell back to an empty state would not.
        assert!(
            painted.paints(name),
            "{name} painted no heading; its text was:\n{}",
            painted.text()
        );
        assert!(
            painted.shapes > 0,
            "{name} drew nothing at all (0 shapes)"
        );
        // The floors below are set from this measurement, read with `--nocapture`:
        // Overview 527 shapes / 5 dots / 21 strings, Articles 16 / 0 / 29, Graph 16 / 6 / 9,
        // Topics 10 / 3 / 19, Monitors 8 / 2 / 15, Chat 9 / 0 / 7. The map's shape count is
        // the outlines' business and moves as the map view's budget does; the floors are set
        // far below it, so a re-budgeted outline does not fail the suite.
        eprintln!(
            "smoke: {name}: shapes={} circles={} strings={}",
            painted.shapes,
            painted.circles,
            painted.text_count()
        );
        seen.push((name, painted));
    });
    // Five pages were asked for; `Chat` is the sixth the window grew, `Reader` the seventh
    // (a story's own page, in the window's engine or as text), drawn last.
    assert_eq!(seen.len(), 7, "the pages of the rail");

    // ---- the map: coastlines and markers are shapes, not text ----
    //
    // Measured: 107 shapes at the world view, of which 6 are the markers. A plate that
    // lost its coastlines — the geojson not found, `World::load` returning nothing, the
    // camera drawing off-screen — keeps only its markers, its hit boxes and the card's
    // frame: tens, not a hundred. The floor is set well below the measurement so a
    // layout tweak does not fail the suite, and well above the marker-only floor so a
    // map that stopped drawing land still does.
    let overview = &seen[0].1;
    assert!(
        overview.shapes >= 60,
        "the map painted only {} shapes for {} markers: the outlines did not reach the \
         plate (107 shapes measured with the vendored outlines present)",
        overview.shapes,
        window.pins.len()
    );
    // The card's own note counts what it painted, so the numbers on screen and the
    // shapes on the plate come from the same read of the same file — and the counts are
    // read back out of the painted text rather than written down here.
    //
    // The note is the map's, and its wording is the map view's business: it used to say
    // "1 632 coastlines · 60 000 points" and now says "6 places · 12 950 segments", so
    // this reads the two counts it paints today — the places it placed, which is the
    // model's own `pins`, and the segments it drew, which can only be non-zero if the
    // outlines were read. Reworded again, this fails with the note it painted.
    let note = overview.text();
    let places = number_before(note, " places");
    assert!(
        places == window.pins.len() as i64,
        "the map painted {places} places for the {} the window holds\n{note}",
        window.pins.len()
    );
    // The geometry count the card prints has been called "segments" and "points" while the
    // map view was reworded; both spellings are read, because what this checks is that the
    // card says it drew outlines at all, not what the line is called.
    let geometry = [" segments", " points"]
        .iter()
        .find_map(|unit| note.contains(unit).then(|| number_before(note, unit)));
    assert!(
        geometry.is_some_and(|count| count > 0),
        "the card says it drew no outline geometry at all, so it loaded no outlines\n{note}"
    );
    // A dot is a place *or* several places close enough on screen to be one dot
    // (`map.rs:699`), and the plate only paints what its camera shows, so the painted dot
    // count is at most the places and at least one — measured 5 dots for 6 places today.
    // The strong statement about the count is the note above, which must equal the model.
    assert!(
        (1..=window.pins.len()).contains(&overview.circles),
        "the map painted {} marker dots for {} places",
        overview.circles,
        window.pins.len()
    );
    // How many story rows fit under the map card is the layout's business (measured: two
    // rows at 1280x800 now, four before the card grew a line), so this asks the weaker
    // question the page must answer on any window: at least one of the stories it was
    // given reaches the screen, with its own title.
    let titles: Vec<&String> = window
        .articles
        .iter()
        .map(|article| &article.title)
        .filter(|title| overview.paints(title))
        .collect();
    assert!(
        !titles.is_empty(),
        "the overview painted none of the {} story titles it was given; it painted:\n{}",
        window.articles.len(),
        overview.text()
    );

    // ---- the graph: nodes are circles, relations are line segments ----
    //
    // Measured: 17 shapes (6 node circles + 7 relation segments + the plate's own
    // chrome) for 8 entities and 7 relations. Six circles, not eight, because the view
    // skips a node whose point falls outside the plate (`src/views/graph.rs:477`) —
    // deliberate culling, so the assertion is that both nodes *and* links were drawn,
    // not that every entity got one.
    let graph = &seen[3].1;
    assert!(
        graph.shapes >= window.entities.len() + window.edges.len(),
        "the graph painted {} shapes for {} entities and {} relations: fewer shapes \
         than nodes plus links means one of the two was not drawn",
        graph.shapes,
        window.entities.len(),
        window.edges.len()
    );
    assert!(
        graph.circles >= 4,
        "the graph drew {} nodes out of {}",
        graph.circles,
        window.entities.len()
    );
    assert!(
        graph.paints("Beta Union"),
        "the relation panel did not paint an entity name"
    );

    // ---- the pages that list rows painted a row of each kind ----
    //
    // By index, because the harness draws them in `PAGES`' order: Overview, Articles,
    // Reader, Graph, Topics, Monitors, Chat.
    assert!(seen[1].1.paints("Alpine rail freight reaches a record"));
    assert!(seen[4].1.paints("Harbour labour"));
    assert!(seen[5].1.paints("Chip export rules"));
}

/// The **real window**: one `Atlas`, built against this test's temporary archive, driven
/// through every page with `set_page` + `draw`.
///
/// This is the half the per-page test above cannot see. `each_page` builds the panel
/// itself, so it never touches the rail, the status row, the frame's read decision
/// (`Atlas::sync`) or the switch from one page to another. Every frame here goes through
/// `Atlas::draw`, which is exactly what the shipped window runs: `eframe::App::update` is
/// one call to it and an `eframe::Frame` the app never reads.
#[test]
fn the_real_window_draws_its_rail_status_row_and_every_page() {
    let temp = TempArchive::build();
    let ctx = context();
    let mut atlas = Atlas::with_archive(temp.file.clone());
    // `with_archive` ends with the same first read `new` does, so the window opens with
    // data in hand: the first frame already paints numbers, not an empty state.
    assert_eq!(atlas.page(), Page::Overview, "the window opens on the overview");

    for page in PAGES {
        atlas.set_page(page);
        assert_eq!(atlas.page(), page);
        let painted = paint_window(&ctx, FRAMES, &mut atlas);

        // ---- the rail: every page it lists, the lockup, the window, Refresh ----
        for listed in PAGES {
            assert!(
                painted.paints(listed.label()),
                "the rail lost its {listed:?} label while showing {page:?}"
            );
        }
        for rail in ["ATLAS", "local news graph", "24 h", "3 d", "7 d", "Refresh"] {
            assert!(
                painted.paints(rail),
                "the rail did not paint {rail:?} while showing {page:?}:\n{}",
                painted.text()
            );
        }
        // …and its four numbers, which are this archive's own, so a rail that painted
        // zeros fails here. `connections` is `Counts::links`, which counts
        // `article_links` (story to story) — five in this archive — and not `edges`,
        // which is the seven relations the graph draws; measured, after this assertion
        // first said "7 connections" and failed.
        for counts in ["8 entities", "5 connections", "3 topics", "2 monitors"] {
            assert!(
                painted.paints(counts),
                "the rail did not paint {counts:?} while showing {page:?}:\n{}",
                painted.text()
            );
        }

        // ---- the status row: which archive, how much, in which window, and what it cost ----
        //
        // The path on screen is `atlas.db`, never the temporary directory it sits in:
        // `app::archive_label` names a file by its components under the checkout, and by
        // its own name otherwise. The window is named beside the count because it is the one
        // word on this row a rail chip changes: the row used to print the archive's whole
        // relevant count, which no window can move, so a working chip read as an inert one.
        assert!(
            painted.paints("atlas.db · 6 stories · 3 d window · read in "),
            "the status row did not name the archive it read while showing {page:?}:\n{}",
            painted.text()
        );
        assert!(
            !painted.paints(&temp.dir.to_string_lossy().to_string()),
            "the status row put a filesystem path on screen: {}\n{}",
            temp.dir.display(),
            painted.text()
        );
        // One read for the whole walk, not one per page — and the walk must never read a
        // second time.
        //
        // **This assertion used to say the opposite** ("page `index` is the `index + 1`-th
        // read"), and that expectation was the defect, not the rule: the page decides what is
        // *drawn*, not what is *queried* (`app::Demand`), so a switch that changes nothing
        // else must not touch the archive. It mattered beyond the query: every read ends in
        // `GraphView::fill`, which frames the plate with zoom 1 and pan 0, so "one read per
        // page" meant Overview → Graph threw the reader's zoom and pan away —
        // `tests/layout_screens.rs` measures that gesture (92.16 px from a fresh picture
        // after a wheel and a drag, 0.00 px after the switch). The window's one read is the
        // one `with_archive` does while it is being built.
        assert!(
            painted.paints("· 1 read"),
            "the status row did not report \"· 1 read\" while showing {page:?}: the page walk \
             read the archive again\n{}",
            painted.text()
        );
        assert!(
            !painted.paints("· 2 read"),
            "a page switch read the archive a second time while showing {page:?}: a page is \
             not part of what a read is for\n{}",
            painted.text()
        );
        let right = if page == Page::Chat {
            "chat needs the local model"
        } else {
            "native window · local archive"
        };
        assert!(
            painted.paints(right),
            "the status row did not say {right:?} while showing {page:?}:\n{}",
            painted.text()
        );

        // ---- the page itself: strings only that page paints ----
        let own: &[&str] = match page {
            Page::Overview => &[
                "World map",
                // The card's own line. Its first clause is **the list in the window** — that is
                // the number a chip press moves (`app.rs`'s status row says the same thing) —
                // and the archive's own size follows it, because the map and the graph are drawn
                // from the archive and not from the window.
                "6 stories in three days · 8 entities · 5 connections in the archive",
            ],
            Page::Articles => &["Choose a story on the left to read it here."],
            // The rail can be clicked before any story is open, so the shell's Reader page is
            // its own invitation here; the page *with* a story is drawn by `each_page` above,
            // which hands `views::reader::show` the first story and reads its words back.
            Page::Reader => &["No page is open."],
            Page::Graph => &["8 entities and 7 relations in the window"],
            Page::Topics => &["Harbour labour"],
            Page::Monitors => &["Chip export rules"],
            Page::Chat => &["Nothing asked yet"],
            Page::Settings => &["The model on this machine"],
        };
        for needle in own {
            assert!(
                painted.paints(needle),
                "the window showed {page:?} but did not paint its page: {needle:?} is \
                 missing:\n{}",
                painted.text()
            );
        }
        assert!(painted.shapes > 0, "{page:?} painted no shapes at all");
    }

    // ---- the rail is not a picture: a click on a label switches the page ----
    atlas.set_page(Page::Overview);
    let painted = paint_window(&ctx, 1, &mut atlas);
    let at = painted
        .rect_of("Topics")
        .expect("the rail painted no Topics label to click")
        .center();
    let after = click(&ctx, &mut atlas, at);
    assert_eq!(
        atlas.page(),
        Page::Topics,
        "a click on the rail's Topics label did not switch the page"
    );
    assert!(
        after.paints("Harbour labour"),
        "the window did not draw the Topics page in the frame the click landed:\n{}",
        after.text()
    );
}

/// No page paints the sentence the window uses to say the archive would not open.
///
/// The window has exactly one warning channel: `src/app.rs:205` keeps the `Err` from
/// `Archive::open`/`reload` and paints it in the status row. So the wording is not
/// recalled here — it is *produced*, by asking the same function for a real failure, and
/// split around the path it names (a fragment that contains this test's temp path could
/// never be painted by a page, so it would make the check vacuous). If `data.rs` rewords
/// the refusal, the fragments follow it and this test keeps its teeth.
///
/// The split point is `assets::label`'s answer, not the absolute path: the refusal names
/// the archive the way the status row may print it — relative to the project root, with a
/// bare file name for anything outside it (`src/assets.rs` `label`, which `data.rs` calls).
/// These temp archives are outside the root on purpose, so the label is `atlas.db`; the
/// assertion that matters is that the sentence is the label *and words around it*, not a
/// path printed raw.
#[test]
fn no_page_paints_the_status_rows_failure_sentence() {
    let absent = std::env::temp_dir().join(format!(
        "atlas-desktop-smoke-absent-{}/atlas.db",
        std::process::id()
    ));
    let failure = match Archive::open(&absent) {
        Ok(_) => panic!("an absent archive opened: {}", absent.display()),
        Err(failure) => failure,
    };
    let named = atlas_desktop::assets::label(&absent);
    assert_eq!(
        named, "atlas.db",
        "a temp archive outside the project root labels as its file name"
    );
    let (before, after) = failure.split_once(&named).unwrap_or_else(|| {
        panic!("the refusal did not name the path it looked at: {failure:?}")
    });
    let (before, after) = (before.trim(), after.trim());
    assert!(
        !before.is_empty() && !after.is_empty(),
        "the refusal is only a path, so there is nothing left to look for: {failure:?}"
    );

    // The window this file draws is a healthy one, so neither fragment is on any page.
    let window = Window::open();
    let ctx = context();
    each_page(&window, |name, page| {
        let painted = paint(&ctx, FRAMES, page);
        for fragment in [before, after] {
            assert!(
                !painted.paints(fragment),
                "{name} painted the archive's failure sentence over a window that \
                 opened: {fragment:?}\n{}",
                painted.text()
            );
        }
    });
}

/// No page paints a warning when the window has data — and the check is not a guess:
/// the sentences it looks for are the ones the *same page* paints when the window is
/// empty, measured in this test rather than listed from memory.
///
/// The guard matters. A page whose empty-state wording changes (or whose wording is
/// deleted) fails here instead of silently making the first assertion vacuous, so the
/// check can never become "the string I remembered is not there".
#[test]
fn each_page_names_its_empty_state_and_a_full_one_paints_none() {
    let window = Window::open();
    let ctx = context();
    let empty = window.empty();

    // page -> the sentence that page paints only when the window has nothing in it.
    let mut empty_states: Vec<(&str, String)> = Vec::new();
    let mut full: Vec<(&str, Painted)> = Vec::new();

    // Overview, empty: the map card says there is nothing to place.
    let mut overview = Overview::new();
    let painted = paint(&ctx, 1, |ui| {
        views::overview::show(ui, &mut overview, &empty);
    });
    empty_states.push(("Overview", only_sentence(&painted, "Nothing collected").to_string()));
    let mut overview_full = Overview::new();
    full.push((
        "Overview",
        paint(&ctx, 1, |ui| {
            views::overview::show(ui, &mut overview_full, &window.snapshot());
        }),
    ));

    // Articles, empty: no row, and no story selected to read.
    let mut articles = ArticlesView::default();
    let painted = paint(&ctx, 1, |ui| {
        views::articles::show(ui, &mut articles, &[], HOURS);
    });
    empty_states.push(("Articles", only_sentence(&painted, "Nothing matches").to_string()));
    let mut articles_full = ArticlesView::default();
    articles_full.selected = window.articles.first().map(|article| article.id);
    full.push((
        "Articles",
        paint(&ctx, 1, |ui| {
            views::articles::show(ui, &mut articles_full, &window.articles, HOURS);
        }),
    ));

    // Graph: with no entities the plate is blank, and the panel has nothing to explain —
    // but it still says what a selection does. That sentence is this page's empty state,
    // and it is *chrome*, not a warning: the same panel paints it with data while nothing
    // is selected.
    //
    // Its wording is read from the pages, not written down here. It used to be spelled
    // "Click a node" in this test, and the panel's sentence changed (it now offers the
    // relation list) — a hard-coded prompt turned a reworded page into a fake defect. The
    // property the test is after is the one measured below: a sentence is painted with
    // nothing, and the same sentence is painted with everything.
    let mut graph = GraphView::default();
    let empty_graph = paint(&ctx, 1, |ui| {
        ui.allocate_ui(egui::vec2(600.0, 620.0), |ui| {
            graph.show(ui, 600.0);
        });
        let _ = views::graph::panel(ui, &graph);
    });
    let mut graph_full = GraphView::default();
    graph_full.fill(window.entities.clone(), window.edges.clone());
    let full_graph = paint(&ctx, 1, |ui| {
        ui.allocate_ui(egui::vec2(600.0, 620.0), |ui| {
            graph_full.show(ui, 600.0);
        });
        let _ = views::graph::panel(ui, &graph_full);
    });
    // The prompt: the longest sentence on the empty panel that the panel with data paints
    // too, and long enough to be a sentence and not a heading.
    let prompt = empty_graph
        .text()
        .lines()
        .filter(|line| line.len() > 24 && line.ends_with('.'))
        .filter(|line| full_graph.paints(line))
        .max_by_key(|line| line.len())
        .unwrap_or_else(|| {
            panic!(
                "the graph panel paints no sentence that survives from an empty window to \
                 a full one; empty:\n{}\nfull:\n{}",
                empty_graph.text(),
                full_graph.text()
            )
        })
        .to_string();
    assert!(
        prompt.len() > 24,
        "the graph panel's prompt is not a sentence: {prompt:?}"
    );
    full.push(("Graph", full_graph));

    // Topics and Monitors, empty: "no topic yet" / "no monitor yet".
    let painted = paint(&ctx, 1, |ui| views::topics::show(ui, &[]));
    empty_states.push(("Topics", only_sentence(&painted, "No topic yet").to_string()));
    full.push((
        "Topics",
        paint(&ctx, 1, |ui| views::topics::show(ui, &window.topics)),
    ));
    let painted = paint(&ctx, 1, |ui| views::monitors::show(ui, &[]));
    empty_states.push(("Monitors", only_sentence(&painted, "No monitor yet").to_string()));
    full.push((
        "Monitors",
        paint(&ctx, 1, |ui| views::monitors::show(ui, &window.monitors)),
    ));

    assert_eq!(empty_states.len(), 4, "four pages have an empty state");
    // The guard: each measured sentence is a whole sentence, and the page that paints
    // it with nothing in the window is the page that must not paint it with data.
    for (page, sentence) in &empty_states {
        assert!(
            sentence.len() > 24 && sentence.ends_with('.'),
            "{page}'s empty state is not a sentence: {sentence:?}"
        );
        let full = full
            .iter()
            .find(|(name, _)| name == page)
            .unwrap_or_else(|| panic!("{page} drew no full window"))
            .1
            .text()
            .to_string();
        // Nothing from the empty state may be painted when the window has data. A
        // page that painted it would be telling the reader the archive is empty while
        // the six stories are on screen.
        assert!(
            !full.contains(sentence),
            "{page} painted its empty state over a window with data: {sentence:?}\n{full}"
        );
        // …and the words of that sentence that name the absence are not there either,
        // so a partial repaint cannot slip through.
        let absence = sentence.split_once(' ').map(|(first, _)| first).unwrap_or("");
        assert!(
            !full.contains(&format!("{absence} ")),
            "{page} painted the beginning of its empty state: {absence:?}"
        );
    }
}

/// The number in `note` immediately before `unit`, with the note's thousands separators
/// stripped: the map card's own count, read back out of what it painted.
///
/// `nf` groups thousands with a narrow space ("12 950 segments"), so a space, a comma and
/// a digit all count as part of the number; the walk stops at the first character that is
/// none of the three.
///
/// Panics with the note when the number is not there, so a reworded card fails with the
/// text it painted instead of with a bare `parse` error.
fn number_before(note: &str, unit: &str) -> i64 {
    let at = note
        .find(unit)
        .unwrap_or_else(|| panic!("the map note never says {unit:?}: {note}"));
    let digits: String = note[..at]
        .chars()
        .rev()
        .take_while(|character| character.is_ascii_digit() || *character == ',' || *character == ' ')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .replace([',', ' '], "")
        .parse()
        .unwrap_or_else(|_| panic!("no number before {unit:?}: {note}"))
}

/// The one painted sentence that starts with `prefix`, so a page's empty state is read
/// from the page rather than written down here.
fn only_sentence<'a>(painted: &'a Painted, prefix: &str) -> &'a str {
    painted
        .text()
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| {
            panic!(
                "no painted sentence starts with {prefix:?}; the page painted:\n{}",
                painted.text()
            )
        })
}

/// The archive really was temporary: this is the file the pages above read, and it is
/// gone when the test ends.
#[test]
fn the_window_under_test_reads_a_temporary_archive() {
    let window = Window::open();
    let path = window.path().to_path_buf();
    assert!(path.is_file(), "{} is not a file", path.display());
    assert!(
        path.starts_with(std::env::temp_dir()),
        "the test archive is outside the temp directory: {}",
        path.display()
    );
    // Not the live archive, by construction — but say it out loud, because "the test
    // opened data/atlas.db" is the mistake this whole file exists to avoid.
    assert!(
        !path.to_string_lossy().contains("/news-atlas/data/"),
        "the test opened the live archive: {}",
        path.display()
    );
    assert_eq!(window.path(), path.as_path());
}
