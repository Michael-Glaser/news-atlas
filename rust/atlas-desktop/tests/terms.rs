//! The first screen: the terms, the acceptance, and what happens after it.
//!
//! Everything here drives the real `Atlas` the window runs, headless, over a temp archive
//! and a temp queue. What is asserted is what a reader can see and what lands on disk:
//!
//!   * a window nobody has accepted anything in shows the document and **no** workspace —
//!     the rail is not there, so there is nothing to click behind it;
//!   * the license on screen is the whole Apache 2.0 text, not a summary of it;
//!   * the acceptance cannot be given without the line, and once given it is a row in the
//!     queue file, which is the window's one writable file;
//!   * a second window over the same file opens the workspace directly, and a *different*
//!     document version asks again.
//!
//! The copy is asserted to be English and to say the three things the user asked for by
//! name: the license, the scope (this app's source, not its dependencies), and the
//! absence of liability.

use std::path::PathBuf;

use egui::{Pos2, Rect, Vec2};

use atlas_desktop::app::Atlas;
use atlas_desktop::requests;
use atlas_desktop::views::license;

/// A temp directory of this run's own. No `tempfile` dependency in this crate: the window
/// links four crates, and a test is not a reason for a fifth.
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("atlas-terms-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp directory");
    dir
}

/// An archive of this test's own: empty, so no page can read the checkout's data.
///
/// The schema is the subset `data.rs` queries, one `CREATE TABLE` per query in that file —
/// the same fixture `tests/smoke.rs` builds, kept here so this file reads on its own.
fn empty_archive(dir: &PathBuf) -> PathBuf {
    let path = dir.join("atlas.db");
    let connection = rusqlite::Connection::open(&path).expect("a fresh database");
    connection
        .execute_batch(
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
    path
}

const SCREEN: Vec2 = Vec2::new(1280.0, 820.0);

fn input(index: usize, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(index as f64 / 60.0),
        events,
        ..Default::default()
    }
}

/// Everything one frame painted, flattened to text and rects.
#[derive(Default)]
struct Painted {
    text: Vec<String>,
    rects: Vec<Rect>,
}

impl Painted {
    fn says(&self, needle: &str) -> bool {
        self.text.iter().any(|line| line.contains(needle))
    }
}

fn walk(shape: &egui::Shape, painted: &mut Painted) {
    match shape {
        egui::Shape::Text(text) => painted.text.push(text.galley.text().to_string()),
        egui::Shape::Rect(rect) => painted.rects.push(rect.rect),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                walk(shape, painted);
            }
        }
        _ => {}
    }
}

fn draw(ctx: &egui::Context, index: usize, events: Vec<egui::Event>, atlas: &mut Atlas) -> Painted {
    let mut output = ctx.run_ui(input(index, events), |ui| atlas.draw(ui));
    output.textures_delta.clear();
    let mut painted = Painted::default();
    for shape in &output.shapes {
        walk(&shape.shape, &mut painted);
    }
    painted
}

#[test]
fn a_fresh_window_shows_the_terms_and_no_workspace() {
    let dir = temp_dir("fresh");
    let queue = dir.join("requests.db");
    let mut atlas = Atlas::with_archive(empty_archive(&dir))
        .with_queue(queue.clone())
        .with_terms();
    assert!(atlas.gate_is_up(), "a window with no acceptance has the gate up");

    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);
    let painted = draw(&ctx, 0, Vec::new(), &mut atlas);

    assert!(
        painted.says("Read this before you start."),
        "the terms screen did not paint its title: {:?}",
        painted.text
    );
    assert!(
        painted.says("Apache License 2.0"),
        "the license is not named on the screen"
    );
    assert!(
        painted.says("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"),
        "the full Apache text is not on screen"
    );
    // …and the workspace is not behind it: no rail, no status row, no page.
    for marker in ["Refresh", "Overview", "local news graph"] {
        assert!(
            !painted.says(marker),
            "the workspace painted behind the terms screen ({marker}): {:?}",
            painted.text
        );
    }
    // Nothing was written by merely looking at it.
    assert!(
        !queue.exists() || !requests::accepted_at(&queue, license::VERSION),
        "looking at the terms recorded an acceptance"
    );
}

#[test]
fn the_screen_says_the_three_things_it_exists_to_say() {
    let all = license::DISCLAIMER.join(" ");
    assert!(all.contains("Apache License 2.0"), "the license is not named");
    assert!(
        all.contains("covers this app's own source only"),
        "the scope of the license is not stated"
    );
    assert!(
        all.contains("libraries it is built on") && all.contains("model itself"),
        "the screen does not say that dependencies and the model keep their own licenses"
    );
    assert!(
        all.contains("no warranty") || all.contains("No warranty"),
        "the screen does not disclaim the warranty"
    );
    assert!(
        all.contains("accept no liability") && all.contains("responsible"),
        "the screen does not say where the responsibility lies"
    );
    assert!(all.contains("can be wrong"), "the screen does not say the model can be wrong");
    // …and it says what leaves the machine, because something does: the search query and the
    // pages it points at. A screen that promised "nothing is sent" would be false the moment
    // the built-in engine answers a search.
    assert!(
        all.contains("leave this machine") && all.contains("search engine"),
        "the screen does not say that the search and the pages it fetches leave the machine"
    );
    // English, and only English: this interface is English end to end.
    for line in license::DISCLAIMER {
        assert!(
            line.is_ascii(),
            "a disclaimer line carries a non-ASCII character: {line:?}"
        );
    }
}

#[test]
fn accepting_writes_one_row_and_the_next_window_opens_the_workspace() {
    let dir = temp_dir("accept");
    let queue = dir.join("requests.db");
    let archive = empty_archive(&dir);

    // The record is written by the same function the screen calls, against the queue file:
    // the window has one writable file, and this is in it.
    requests::accept_at(&queue, license::VERSION).expect("the acceptance is recorded");
    assert!(requests::accepted_at(&queue, license::VERSION));
    assert!(
        !requests::accepted_at(&queue, "apache-2.0+notice-0"),
        "an acceptance answered for a document it was not given"
    );

    let mut atlas = Atlas::with_archive(archive)
        .with_queue(queue)
        .with_terms();
    assert!(
        !atlas.gate_is_up(),
        "a window over an accepted queue still showed the terms"
    );
    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);
    let painted = draw(&ctx, 0, Vec::new(), &mut atlas);
    assert!(
        painted.says("Refresh"),
        "the workspace did not paint after an acceptance: {:?}",
        painted.text
    );
    assert!(!painted.says("Read this before you start."));
}

#[test]
fn the_acceptance_is_a_row_in_the_one_file_the_window_writes() {
    let dir = temp_dir("row");
    let queue = dir.join("requests.db");
    requests::accept_at(&queue, license::VERSION).expect("accepted");
    // A second acceptance of the same version is an update, not a second row: the record is
    // one answer per document.
    requests::accept_at(&queue, license::VERSION).expect("accepted twice");
    let connection = rusqlite::Connection::open(&queue).expect("the queue file");
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM acceptance", [], |row| row.get(0))
        .expect("a count");
    assert_eq!(rows, 1, "the acceptance was recorded more than once");
    let stamp: String = connection
        .query_row("SELECT at FROM acceptance", [], |row| row.get(0))
        .expect("the stamp");
    assert!(
        stamp.len() == 20 && stamp.ends_with('Z') && stamp.contains('T'),
        "the acceptance is stamped in the archive's own format: {stamp:?}"
    );
}

#[test]
fn the_gate_cannot_be_answered_without_the_line() {
    let mut gate = license::Gate::default();
    assert!(!gate.ready(), "an untouched gate offered the door");
    gate.checked = true;
    assert!(gate.ready());
    // "Read to the end" is a hint, not a lock: the Apache text is 176 lines and a reader may
    // accept from the first page. What is not optional is the line itself.
    gate.reached_end = false;
    assert!(gate.ready(), "the hint became a lock");
}

#[test]
fn the_embedded_license_is_the_whole_apache_text() {
    assert!(
        license::TEXT.starts_with("                              Apache License"),
        "the embedded text does not open like the Apache license"
    );
    assert!(license::TEXT.contains("Version 2.0, January 2004"));
    assert!(license::TEXT.contains("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"));
    assert!(license::TEXT.contains("END OF TERMS AND CONDITIONS"));
    assert!(
        license::TEXT.trim_end().ends_with("limitations under the License."),
        "the embedded text does not end like the Apache license"
    );
    assert!(
        license::TEXT.lines().count() > 150,
        "the embedded license is {} lines, which is a summary",
        license::TEXT.lines().count()
    );
    // The repository's own LICENSE is the same bytes: one text, two places, no drift.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let on_disk = std::fs::read_to_string(root.join("LICENSE")).expect("the repository LICENSE");
    assert_eq!(
        on_disk.trim_end(),
        license::TEXT.trim_end(),
        "the LICENSE file and the screen's embedded text have drifted"
    );
}
