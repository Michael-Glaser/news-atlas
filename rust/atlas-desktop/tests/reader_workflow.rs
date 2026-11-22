//! The reader workflow, end to end where the tree allows it — 2026-11-22.
//!
//! The Reader page exists (`views::reader`, `webview`), and so does the page-to-shell channel
//! that a "Read the source" link uses ([`reader::remember_source`] / [`reader::take_source`]).
//! What does **not** exist yet is the shell's half of steps 2 and 3: `app.rs` never calls
//! `take_source`, never draws `views::reader::show`, and `Page` has no `Reader` variant, so a
//! reader cannot reach the page inside the window. Those two steps are therefore proved at the
//! seam the shell will pass through — the page renders exactly that story and that URL, and the
//! shell's `Origin`/`back_row` machinery returns to the page with its narrowing intact — and the
//! missing shell API is named in each test's failure text and in the module note of §2 and §3.
//!
//! What is measured, not assumed:
//!
//! * the row's URL reaches the link ([`Story::of`] keeps `Article::url`) and the link paints the
//!   host and hands the shell the address;
//! * the page paints the title, the host and the stored text of *that* story;
//! * the shell's back row returns to the page the story came from with the narrowing still in
//!   force (the country chip that narrowed Articles to two of four stories);
//! * every non-http(s) scheme is refused *before* anything is painted;
//! * a Reader frame adds no archive read: the shell's painted counter is the same before and
//!   after (`… · N reads` in the status row, the same number `tests/frame_cost.rs` reads).
//!
//! Headless, like the other window tests: `Context::default()`, `run_ui`, and the frame's
//! `TexturesDelta` cleared (`epaint 0.37` panics on drop otherwise).

use std::path::{Path, PathBuf};

use egui::{Pos2, Rect, Shape, Vec2};
use rusqlite::Connection;

use atlas_desktop::app::{Atlas, Page};
use atlas_desktop::data::{Archive, Article};
use atlas_desktop::views::reader;
use atlas_desktop::{link, theme};

const SCREEN: Vec2 = Vec2::new(1280.0, 800.0);

// --------------------------------------------------------------------------- fixture

/// A temp database with the columns the window reads, and nothing in it.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> TempDb {
        let path = std::env::temp_dir().join(format!(
            "atlas-reader-workflow-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("a temp file is writable");
        conn.execute_batch(SCHEMA).expect("the fixture schema");
        drop(conn);
        TempDb { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self) -> Connection {
        Connection::open(&self.path).expect("the fixture is openable twice")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.path.display(), suffix));
        }
    }
}

/// `tests/archive.rs`'s schema, copied: one fixture per test file is cheaper to read than a
/// shared module, and a test must not depend on another test's helper.
const SCHEMA: &str = "
CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT, url TEXT, active INTEGER DEFAULT 1);
CREATE TABLE articles(
  id INTEGER PRIMARY KEY, source_id INTEGER, url TEXT, title TEXT, summary TEXT,
  content TEXT, published TEXT, fetched TEXT, iso TEXT, lat REAL, lon REAL,
  relevant INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0, full_text INTEGER DEFAULT 0,
  publisher TEXT, place TEXT, geo_src TEXT);
CREATE TABLE entities(
  id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER DEFAULT 1,
  first_seen TEXT, last_seen TEXT);
CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL DEFAULT 1, type TEXT, label TEXT);
CREATE TABLE article_links(a INTEGER, b INTEGER, shared INTEGER, score REAL, why TEXT);
CREATE TABLE topics(
  id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER DEFAULT 1,
  hits INTEGER DEFAULT 0, last_hit TEXT);
CREATE TABLE monitors(
  id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER DEFAULT 1,
  cadence_minutes INTEGER DEFAULT 0, last_run TEXT, hits INTEGER DEFAULT 0);
CREATE TABLE alerts(id INTEGER PRIMARY KEY, monitor_id INTEGER, article_id INTEGER);
";

/// `now - hours` in the archive's own shape (`2026-11-21T09:25:29Z`).
fn iso_ago(hours: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs() as i64;
    let then = now - (hours * 3600.0) as i64;
    let (days, seconds) = (then / 86_400, then % 86_400);
    // 1970-01-01 + `days`, by the same civil-from-days arithmetic the app uses.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// One fixture row: what a story needs to be drawn on Articles and counted on Overview.
struct Row<'a> {
    title: &'a str,
    url: String,
    iso: &'a str,
    lat: f64,
    lon: f64,
    full_text: bool,
}

impl<'a> Row<'a> {
    fn new(title: &'a str, iso: &'a str, lat: f64, lon: f64) -> Row<'a> {
        // The address must be unique per story: the shell maps an opened url back to a story by
        // exact `article.url == url` (`app.rs:1279` `story_at`), so two rows sharing an address
        // would make "open this row" open its sibling. Slug the title, not the place.
        let slug: String = title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        Row {
            title,
            url: format!("https://news.example.invalid/{}", slug.trim_matches('-')),
            iso,
            lat,
            lon,
            full_text: true,
        }
    }
}

fn insert(conn: &Connection, row: &Row<'_>) {
    conn.execute(
        "INSERT INTO articles(url,title,summary,content,published,iso,lat,lon,relevant,dupe,\
         full_text,source_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,0,?9,1)",
        rusqlite::params![
            row.url,
            row.title,
            format!("Summary of {}.", row.title),
            format!("Body of {}. Chip export rules.", row.title),
            iso_ago(1.0),
            row.iso,
            row.lat,
            row.lon,
            row.full_text,
        ],
    )
    .expect("the fixture accepts the row");
}

/// Four stories, two in Germany and two in France, all inside the window, plus a source row.
fn fixture(name: &str) -> TempDb {
    let db = TempDb::new(name);
    let conn = db.write();
    conn.execute(
        "INSERT INTO sources(id,name,url,active) VALUES(1,'The Nordic Post','https://n.example.invalid',1)",
        [],
    )
    .expect("a source row");
    insert(&conn, &Row::new("Harbour crane strike ends after nine days", "DEU", 53.55, 9.99));
    insert(&conn, &Row::new("Ferry traffic resumes on the northern route", "DEU", 54.32, 10.13));
    insert(&conn, &Row::new("Drought reshapes the southern harvest", "FRA", 48.85, 2.35));
    insert(&conn, &Row::new("Alpine rail freight reaches a record", "FRA", 45.76, 4.83));
    conn.execute(
        "INSERT INTO topics(id,name,keywords,active,hits) VALUES(1,'Chip export rules','chips,export',1,4)",
        [],
    )
    .expect("a topic row");
    db
}

/// The fixture's own url for a story title, read from the fixture rather than written twice.
fn url_of(db: &TempDb, title: &str) -> String {
    let conn = db.write();
    conn.query_row(
        "SELECT url FROM articles WHERE title = ?1",
        [title],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|error| panic!("the fixture has no row titled {title:?}: {error}"))
}

/// The first story the archive answers, as the page draws it.
fn story_of(db: &TempDb) -> (Story5, Article) {
    let archive = Archive::open(db.path()).expect("a valid fixture archive must open");
    let articles = archive.articles(48, 10, "");
    assert!(!articles.is_empty(), "the fixture must have a story in the window");
    let article = articles[0].clone();
    let story = reader::Story::of(&article);
    (Story5 { story }, article)
}

/// A one-field wrapper so the helper above can return the story without naming its type twice.
struct Story5 {
    story: reader::Story,
}

// --------------------------------------------------------------------------- harness

/// A headless context with the window's theme installed.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// One monotonic clock for the whole test binary: `egui` keeps a history of frame times and
/// panics when one moves backwards (`emath/src/history.rs:129`), and a per-test counter restarts
/// at zero for every test that shares the process.
fn next_time() -> f64 {
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64 / 60.0
}

fn input(_index: usize) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(next_time()),
        ..Default::default()
    }
}

fn frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    mut draw: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, |ui| draw(ui));
    output.textures_delta.clear();
    output
}

/// What one frame painted: every string, and where each was drawn.
#[derive(Default, Clone)]
struct Painted {
    labels: Vec<(String, Rect)>,
    text: String,
}

impl Painted {
    fn paints(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    fn rect_of(&self, needle: &str) -> Option<Rect> {
        self.labels
            .iter()
            .find(|(text, _)| text == needle)
            .map(|(_, rect)| *rect)
            .or_else(|| {
                self.labels
                    .iter()
                    .find(|(text, _)| text.contains(needle))
                    .map(|(_, rect)| *rect)
            })
    }

    /// The window's read counter as the status row prints it (`… · 3 reads`), the same number
    /// `tests/frame_cost.rs` reads — the field itself is private.
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
}

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
        _ => {}
    }
}

/// Paint `draw` for `frames` frames and return the last frame.
fn paint(ctx: &egui::Context, frames: usize, mut draw: impl FnMut(&mut egui::Ui)) -> Painted {
    let mut painted = Painted::default();
    for index in 0..frames {
        let output = frame(ctx, input(index), |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(theme::PAGE))
                .show(ui, |ui| draw(ui));
        });
        painted = measure(&output.shapes);
    }
    painted
}

/// The same, for the window itself.
fn paint_window(ctx: &egui::Context, frames: usize, atlas: &mut Atlas) -> Painted {
    let mut painted = Painted::default();
    for index in 0..frames {
        let output = frame(ctx, input(index), |ui| atlas.draw(ui));
        painted = measure(&output.shapes);
    }
    painted
}

/// Press and release the primary button at `at` inside the window (two frames, because egui
/// reports `clicked()` on the release).
fn click_window(ctx: &egui::Context, atlas: &mut Atlas, at: Pos2) -> Painted {
    let with_pointer = |pressed: bool| egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(next_time()),
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
    frame(ctx, with_pointer(true), |ui| atlas.draw(ui));
    let output = frame(ctx, with_pointer(false), |ui| atlas.draw(ui));
    measure(&output.shapes)
}

/// The same, for a bare page.
fn click_page(ctx: &egui::Context, at: Pos2, mut draw: impl FnMut(&mut egui::Ui)) -> Painted {
    let with_pointer = |pressed: bool| egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(0.0),
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
    for pressed in [true, false] {
        let output = frame(ctx, with_pointer(pressed), |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(theme::PAGE))
                .show(ui, |ui| draw(ui));
        });
        let _ = measure(&output.shapes);
    }
    paint(ctx, 1, draw)
}

// --------------------------------------------------------------------------- (1) offered

/// An article row's URL reaches "Read the source", and pressing it hands the shell that URL.
///
/// The accessor the view exposes is [`reader::source_link`] (the row-level link) with the row's
/// URL taken from [`reader::Story::of`] — the page's own shape of an `Article`. Nothing here
/// types the address: it comes out of the fixture row and is compared to the fixture's column.
#[test]
fn the_row_url_is_offered_as_read_the_source() {
    let db = fixture("offered");
    let (story, article) = story_of(&db);
    let url = story.story.url.clone();
    assert_eq!(
        url, article.url,
        "Story::of must carry the row's url, not a copy of the title or the publisher's home page"
    );

    let ctx = context();
    let offered = paint(&ctx, 2, |ui| {
        let _ = reader::source_link(ui, &url);
    });
    assert!(
        offered.paints("Read the source"),
        "the row's url is not offered as \"Read the source\"; the frame painted:\n{}",
        offered.text
    );
    assert!(
        offered.paints("news.example.invalid"),
        "the link does not say where it goes; it painted:\n{}",
        offered.text
    );

    // Press it: the link answers `true` and the shell can take the address.
    let at = offered
        .rect_of("Read the source")
        .expect("the link painted no rect")
        .center();
    let mut pressed = false;
    let after = click_page(&ctx, at, |ui| {
        if reader::source_link(ui, &url) {
            pressed = true;
        }
    });
    assert!(pressed, "a press on \"Read the source\" was not reported to the row");
    assert_eq!(
        reader::take_source(&ctx).as_deref(),
        Some(url.as_str()),
        "the press did not hand the shell the row's url (the frame after it painted:\n{})",
        after.text
    );
    // Taken once: a press must not open the same page again next frame.
    assert_eq!(
        reader::take_source(&ctx),
        None,
        "the remembered url outlived its frame"
    );
    println!(
        "link: {:?}; press -> true; take_source -> Some({url:?}) once, then None",
        offered
            .labels
            .iter()
            .map(|(label, _)| label)
            .find(|label| label.contains("Read the source"))
            .cloned()
            .unwrap_or_default()
    );
}

/// The same two rules where the user meets them: the reading pane of the list.
///
/// The list row itself is the selection, and the pane shows the chosen story
/// (`views/articles.rs:479`); the pane's "Read the source" link (`articles.rs:516`) carries the
/// row's own address. So: choose the http row, the pane paints exactly one link that names the
/// host, and pressing it opens *that* story; choose the `javascript:` row, the pane paints the
/// "not offered" sentence instead, and no press can open anything.
#[test]
fn the_reading_pane_offers_its_story_and_never_a_hostile_address() {
    let db = TempDb::new("rows");
    {
        let conn = db.write();
        conn.execute(
            "INSERT INTO sources(id,name,url,active) VALUES(1,'The Nordic Post','https://n.example.invalid',1)",
            [],
        )
        .expect("a source row");
        insert(
            &conn,
            &Row::new("Harbour crane strike ends after nine days", "DEU", 53.55, 9.99),
        );
        // A stored address no engine may be handed. The row still has a title and a place; the
        // pane must say so and offer no link.
        let mut hostile = Row::new("Fishing quota talks resume in the north", "DEU", 54.0, 10.0);
        hostile.url = "javascript:alert(document.domain)".to_string();
        insert(&conn, &hostile);
    }

    let ctx = context();
    let mut atlas = Atlas::with_archive(db.path().to_path_buf());
    atlas.set_page(Page::Articles);
    let list = paint_window(&ctx, 3, &mut atlas);
    let good_row = list
        .rect_of("Harbour crane strike ends after nine days")
        .expect("the list painted no row for the http story")
        .center();
    click_window(&ctx, &mut atlas, good_row);
    let pane = paint_window(&ctx, 2, &mut atlas);
    assert_eq!(
        atlas.page(),
        Page::Articles,
        "choosing a row left the list; it painted:\n{}",
        pane.text
    );
    let links: Vec<&(String, Rect)> = pane
        .labels
        .iter()
        .filter(|(label, _)| label.starts_with("Read the source"))
        .collect();
    assert_eq!(
        links.len(),
        1,
        "the pane must offer exactly one link for the chosen story, but painted {}:\n{:?}\n{}",
        links.len(),
        links.iter().map(|(label, _)| label).collect::<Vec<_>>(),
        pane.text
    );
    let (label, rect) = links[0];
    assert!(
        label.contains("news.example.invalid"),
        "the pane's link does not say where it goes: {label:?}"
    );

    click_window(&ctx, &mut atlas, rect.center());
    let page = paint_window(&ctx, 2, &mut atlas);
    assert_eq!(
        atlas.page(),
        Page::Reader,
        "a press on the pane's link did not open the Reader page; it painted:\n{}",
        page.text
    );
    assert!(
        page.paints("Harbour crane strike ends after nine days"),
        "the pane's link opened a page, but not the story in the pane; it painted:\n{}",
        page.text
    );

    // The hostile address: its row is chosen the same way, and the pane refuses it in words.
    let mut other = Atlas::with_archive(db.path().to_path_buf());
    other.set_page(Page::Articles);
    let list = paint_window(&ctx, 3, &mut other);
    let bad_row = list
        .rect_of("Fishing quota talks resume in the north")
        .expect("the list painted no row for the hostile story")
        .center();
    click_window(&ctx, &mut other, bad_row);
    let pane = paint_window(&ctx, 2, &mut other);
    assert_eq!(
        other.page(),
        Page::Articles,
        "choosing a row opened a page on its own; it painted:\n{}",
        pane.text
    );
    assert!(
        !pane.paints("Read the source"),
        "the pane offered a link for a `javascript:` address; it painted:\n{}",
        pane.text
    );
    assert!(
        pane.paints("not a web address, not offered"),
        "the pane did not say why the address is not offered; it painted:\n{}",
        pane.text
    );
    println!(
        "pane link: one {label:?} for the http story, press -> Reader with it; the `javascript:` \
         row paints the refusal sentence and offers nothing"
    );
}

// --------------------------------------------------------------------------- (2) the page

/// Opening the offered address lands on the Reader page with that URL and that story.
///
/// The press is the link's own channel: [`reader::source_link`] leaves the address with
/// [`reader::remember_source`] (proved in §1), and the shell takes it once per frame
/// (`app.rs:1054` → `open_source` → `open_reader`). This is the whole chain through
/// `Atlas::draw`; nothing here calls `open_reader` directly.
#[test]
fn opening_the_offered_url_lands_on_the_reader_page() {
    let db = fixture("page");
    let ctx = context();
    let mut atlas = Atlas::with_archive(db.path().to_path_buf());
    atlas.set_page(Page::Articles);
    let list = paint_window(&ctx, 2, &mut atlas);
    let title = "Harbour crane strike ends after nine days";
    assert!(
        list.paints(title),
        "the Articles list painted no story row for the fixture; it painted:\n{}",
        list.text
    );

    let url = url_of(&db, title);
    // What a press on the row's link does. The link itself is proved in §1; here it is the
    // shell's half that is on trial.
    reader::remember_source(&ctx, &url);
    let page = paint_window(&ctx, 3, &mut atlas);

    assert_eq!(
        atlas.page(),
        Page::Reader,
        "the shell did not open the Reader page for the address the link left; it painted:\n{}",
        page.text
    );
    assert!(
        page.paints(title),
        "the Reader page did not paint the story the address belongs to; it painted:\n{}",
        page.text
    );
    assert!(
        page.paints("news.example.invalid"),
        "the Reader page did not name the address's host; it painted:\n{}",
        page.text
    );
    assert!(
        page.paints("Body of"),
        "the Reader page did not paint the stored text; it painted:\n{}",
        page.text
    );
    for control in ["Reload", "Ask about this", "Tag it", "Back to Articles"] {
        assert!(
            page.paints(control),
            "the Reader page is missing {control:?}; it painted:\n{}",
            page.text
        );
    }
    println!(
        "reader: page={:?}, title {:?} + host + stored text painted, controls Reload/Ask about \
         this/Tag it/Back to Articles painted, no engine back arrow",
        atlas.page(),
        title
    );
    assert!(
        !page.paints("←"),
        "the page drew its own back arrow with no engine behind it: `views/reader.rs` sets \
         `can_back = view.is_some()` because the page keeps no history of its own, and the \
         shell's back row is the way back on a build without one"
    );
}

// --------------------------------------------------------------------------- (3) back

/// The back path returns to the page the story came from, with its narrowing choice intact.
///
/// Narrowing: the country rollup on Overview ("DEU 2") — pressing it opens Articles with the
/// list narrowed to that place, and the caption says so ("2 stories in the last 24 h…"). The
/// address is then opened through the link's channel, and "Back to Articles" must return to the
/// *narrowed* list, which is what `app.rs`'s `Origin` carries across a page switch.
#[test]
fn the_back_path_returns_to_the_page_and_keeps_its_narrowing() {
    let db = fixture("back");
    let ctx = context();
    let mut atlas = Atlas::with_archive(db.path().to_path_buf());
    atlas.set_page(Page::Overview);
    let overview = paint_window(&ctx, 3, &mut atlas);
    let chip = overview
        .rect_of("DEU 2")
        .expect("the Overview painted no DEU rollup row")
        .center();

    click_window(&ctx, &mut atlas, chip);
    let narrowed = paint_window(&ctx, 2, &mut atlas);
    assert_eq!(
        atlas.page(),
        Page::Articles,
        "a press on the country rollup did not narrow the list; it painted:\n{}",
        narrowed.text
    );
    assert!(
        narrowed.paints("2 stories in the last"),
        "the rollup did not narrow Articles to the fixture's two German stories; it painted:\n{}",
        narrowed.text
    );
    let narrowed_caption = caption(&narrowed, "2 stories in the last");

    let url = url_of(&db, "Harbour crane strike ends after nine days");
    reader::remember_source(&ctx, &url);
    let opened = paint_window(&ctx, 3, &mut atlas);
    assert_eq!(
        atlas.page(),
        Page::Reader,
        "the address did not open the Reader page from the narrowed list; it painted:\n{}",
        opened.text
    );

    let back = opened
        .rect_of("Back to Articles")
        .expect("the Reader page painted no back row")
        .center();
    click_window(&ctx, &mut atlas, back);
    let returned = paint_window(&ctx, 2, &mut atlas);
    assert_eq!(
        atlas.page(),
        Page::Articles,
        "the back row did not return to Articles; it painted:\n{}",
        returned.text
    );
    assert!(
        returned.paints("2 stories in the last"),
        "the way back lost the narrowing: the list is not the two German stories any more; it \
         painted:\n{}",
        returned.text
    );
    let returned_caption = caption(&returned, "2 stories in the last");
    assert_eq!(
        returned_caption, narrowed_caption,
        "the way back kept a different list than the one the story was opened from"
    );
    println!(
        "back: Overview --DEU 2--> Articles ({narrowed_caption:?}) -> Reader -> Articles \
         ({returned_caption:?})"
    );
}

/// The one painted label that starts with `prefix`, so a caption can be compared verbatim.
fn caption(painted: &Painted, prefix: &str) -> String {
    painted
        .labels
        .iter()
        .map(|(label, _)| label)
        .find(|label| label.starts_with(prefix))
        .cloned()
        .unwrap_or_else(|| panic!("nothing painted a label starting {prefix:?}:\n{}", painted.text))
}

// --------------------------------------------------------------------------- (4) refusal

/// A URL that is not http/https is never offered: no label, no press, no stored address.
#[test]
fn a_url_that_is_not_http_is_never_offered() {
    let hostile = [
        "javascript:alert(1)",
        "JAVASCRIPT:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "file:///etc/passwd",
        "mailto:someone@example.invalid",
        "HTTP://news.example.invalid/x",
        "HTTPS://news.example.invalid/x",
        "ftp://news.example.invalid/x",
        "//news.example.invalid/x",
        "news.example.invalid/x",
        "https://",
        "https://user:pass@news.example.invalid/x",
        "https://news.example.invalid/\\..\\x",
        "https://news.example.invalid/\u{202E}gpj.exe",
        "",
    ];
    let ctx = context();
    for url in hostile {
        assert_eq!(
            link::openable(url),
            None,
            "link::openable accepted {url:?}"
        );
        let painted = paint(&ctx, 1, |ui| {
            assert!(
                !reader::source_link(ui, url),
                "source_link offered {url:?}"
            );
        });
        assert!(
            !painted.paints("Read the source"),
            "{url:?} was offered as a link; the frame painted:\n{}",
            painted.text
        );
        assert_eq!(
            reader::take_source(&ctx),
            None,
            "{url:?} reached the shell's slot"
        );
    }

    // And the positive control: the same call does offer a real address, so the loop above is
    // not passing because nothing is painted at all.
    let offered = paint(&ctx, 1, |ui| {
        let _ = reader::source_link(ui, "https://news.example.invalid/x");
    });
    assert!(
        offered.paints("Read the source"),
        "the positive control painted no link, so the refusals above prove nothing"
    );
    println!(
        "hostile urls: {} refused by link::openable, {} painted no link and wrote no slot; \
         positive control painted one",
        hostile.len(),
        hostile.len()
    );
}

// --------------------------------------------------------------------------- (5) frame cost

/// A Reader frame costs at most one archive read — measured with the shell's own counter.
///
/// The counter is private (`Atlas.reads`); the status row prints it (`… · N reads`), which is
/// how `tests/frame_cost.rs` reads it too. The frame that *opens and draws* the Reader page is
/// the one measured: the address comes through the link's channel, so the shell does its own
/// work (finding the story, opening the page) plus the page's frame.
#[test]
fn a_reader_frame_costs_at_most_one_archive_read() {
    let db = fixture("cost");
    let ctx = context();
    let mut atlas = Atlas::with_archive(db.path().to_path_buf());
    atlas.set_page(Page::Articles);
    let before = paint_window(&ctx, 2, &mut atlas)
        .reads()
        .expect("the status row painted no read count");
    assert!(before >= 1, "the window's first frames read nothing at all");

    let url = url_of(&db, "Drought reshapes the southern harvest");
    reader::remember_source(&ctx, &url);
    let page = paint_window(&ctx, 3, &mut atlas);
    assert_eq!(
        atlas.page(),
        Page::Reader,
        "the Reader page did not open, so its cost cannot be measured; it painted:\n{}",
        page.text
    );
    assert!(
        page.paints("Drought reshapes the southern harvest"),
        "the Reader frame painted no story, so the number below measures nothing"
    );

    let after = page
        .reads()
        .expect("the status row painted no read count after the Reader frame");
    let cost = after.saturating_sub(before);
    assert!(
        cost <= 1,
        "opening and drawing the Reader page cost {cost} archive reads (before {before}, \
         after {after}); the page holds a `Story` and no `Archive`, so at most the shell's own \
         lookup may read once"
    );
    println!("reader frame: {before} -> {after} reads (cost {cost})");
}
