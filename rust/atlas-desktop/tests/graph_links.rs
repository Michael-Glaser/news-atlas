//! The Graph page's relation rows are doors: pinned end to end.
//!
//! Three claims, and each one is checked at the level it is made:
//!
//! * **A relation row is clickable.** The panel is drawn in a headless `egui::Context` with
//!   a real `PointerButton` press and release, at the positions the panel actually paints
//!   at — the rows are walked from the top of the panel down, and the click that lands on
//!   the relation row is the one that answers the pair `(0, 1)`. No rect is assumed: the
//!   sweep is what makes the assertion true of the drawn panel rather than of a guess about
//!   its layout.
//! * **Its stories are the ones that mention both entities.** `stories_for` is asked
//!   directly, from both ends, and the answer is compared with a fixture whose rows are
//!   known by hand — including the rows that must *not* appear: a story naming only one end,
//!   a duplicate, and a story the model did not call news.
//! * **Clicking a story returns its id.** The same sweep opens every story the panel offers,
//!   in the order the rows are drawn, and the ids come back in published order — newest
//!   first, `10, 11, 12, 17` — through both doors at once: `panel`'s return value on the
//!   frame of the click, and `GraphView::show`'s on the frame after it. The two must agree,
//!   because the shell draws the canvas before the panel and only ever sees `show`.
//!
//! Nothing here opens a window, touches the live `data/atlas.db`, or listens on anything.
//! The archive is a fixture under the system temp directory, seeded with SQL and closed
//! before the view is pointed at it, so every query the panel runs is a read-only one.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use atlas_desktop::data::{Edge, Entity};
use atlas_desktop::views::graph::{panel, stories_for, GraphView, PANEL_WIDTH};
use egui::{Pos2, Rect, Vec2};
use rusqlite::Connection;

// --------------------------------------------------------------------------- fixture

/// A fresh directory under the system temp directory, and the empty archive file in it.
fn temp_archive(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "atlas-graph-links-{tag}-{}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp directory");
    dir.join("atlas.db")
}

/// The tables and columns the panel's one statement names, and nothing else.
fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE articles(
             id INTEGER PRIMARY KEY, source_id INTEGER, title TEXT, published TEXT,
             relevant INTEGER DEFAULT 0, dupe INTEGER DEFAULT 0);
         CREATE TABLE entities(
             id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER);
         CREATE TABLE article_entities(
             article_id INTEGER, entity_id INTEGER, PRIMARY KEY(article_id, entity_id));
         CREATE TABLE edges(
             a INTEGER, b INTEGER, weight REAL, type TEXT, label TEXT,
             PRIMARY KEY(a, b, type));",
    )
    .expect("the archive's shape");
}

/// The stories that name both Ada Lovelace (1) and Isambard Brunel (2), and the four
/// neighbours that must not be answered as if they did.
///
/// The pairings are 10, 11, 12 and 17. 13 names only Ada, 14 names only Brunel, 15 names
/// both but is a duplicate of a wire copy (the reading pane will not show it), and 16 names
/// both but the model did not call it news (`relevant = 0`). 15 and 16 are both *newer*
/// than every answer, so a predicate that forgot either filter would put them first and the
/// order assertion would fail, not just the membership one.
fn seed(path: &Path) {
    let conn = Connection::open(path).expect("a writable copy to seed");
    schema(&conn);
    conn.execute("INSERT INTO sources(id, name) VALUES (1, 'The Engineer')", [])
        .expect("a source");
    let stories = [
        (10, "Ships and engines", "2026-11-21T09:00:00Z", 1, 0),
        (11, "Bridge over the Avon", "2026-11-21T08:00:00Z", 1, 0),
        (12, "Ada and Brunel, once more", "2026-11-21T07:00:00Z", 1, 0),
        (13, "Ada alone", "2026-11-21T09:30:00Z", 1, 0),
        (14, "Brunel alone", "2026-11-21T09:10:00Z", 1, 0),
        (15, "The same wire copy", "2026-11-21T10:00:00Z", 1, 1),
        (16, "A page the model did not call news", "2026-11-21T10:30:00Z", 0, 0),
        (17, "An older pairing", "2026-11-19T10:00:00Z", 1, 0),
    ];
    for (id, title, published, relevant, dupe) in stories {
        conn.execute(
            "INSERT INTO articles(id, source_id, title, published, relevant, dupe) \
             VALUES (?1, 1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, title, published, relevant, dupe],
        )
        .expect("a story");
    }
    conn.execute(
        "INSERT INTO entities(id, name, type, count) \
         VALUES (1, 'Ada Lovelace', 'PERSON', 5), (2, 'Isambard Brunel', 'PERSON', 3)",
        [],
    )
    .expect("two entities");
    // The mentions: 13 has one end, 14 has the other, everything else in the pair has both.
    let mentions = [
        (10, 1),
        (10, 2),
        (11, 1),
        (11, 2),
        (12, 1),
        (12, 2),
        (13, 1),
        (14, 2),
        (15, 1),
        (15, 2),
        (16, 1),
        (16, 2),
        (17, 1),
        (17, 2),
    ];
    for (article, entity) in mentions {
        conn.execute(
            "INSERT INTO article_entities(article_id, entity_id) VALUES (?1, ?2)",
            rusqlite::params![article, entity],
        )
        .expect("a mention");
    }
    conn.execute(
        "INSERT INTO edges(a, b, weight, type, label) VALUES (1, 2, 4.0, 'cooccur', '')",
        [],
    )
    .expect("one relation");
}

/// The two entities the relation joins, in the order `fill` numbers them: slot 0 is Ada,
/// slot 1 is Brunel.
fn entities() -> Vec<Entity> {
    vec![
        Entity { id: 1, name: "Ada Lovelace".into(), kind: "PERSON".into(), count: 5 },
        Entity { id: 2, name: "Isambard Brunel".into(), kind: "PERSON".into(), count: 3 },
    ]
}

fn edges() -> Vec<Edge> {
    vec![Edge { a: 1, b: 2, weight: 4.0, label: String::new() }]
}

// --------------------------------------------------------------------------- frames

/// What one drawn frame answered, and where the panel was drawn.
///
/// The two half-answers are the two doors a story can come through: `canvas` is
/// [`GraphView::show`]'s (the one the shell reads) and `panel` is [`panel`]'s (the same
/// story, one frame earlier).
struct Drawn {
    canvas: Option<i64>,
    panel: Option<i64>,
    /// The panel's own box, measured in the frame that drew it: every click below is aimed
    /// from here rather than from an assumption about margins and row heights.
    rect: Rect,
}

/// One frame of the page, in the order the shell draws it: the canvas, then the panel.
fn frame(ctx: &egui::Context, events: Vec<egui::Event>, view: &mut GraphView) -> Drawn {
    let mut drawn = Drawn {
        canvas: None,
        panel: None,
        rect: Rect::NOTHING,
    };
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(PANEL_WIDTH + 200.0, 720.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            // A page has a width: without one the panel's sentences would not wrap and the
            // frame would be wider than any window this app opens.
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    // The canvas first, as the page draws it: a click in the panel therefore
                    // reaches `show` on the frame after it, which is the contract under test.
                    ui.allocate_ui(Vec2::new(120.0, 120.0), |ui| {
                        drawn.canvas = view.show(ui, 120.0);
                    });
                    let box_ = ui.allocate_ui(Vec2::new(PANEL_WIDTH, 560.0), |ui| {
                        drawn.panel = panel(ui, view);
                    });
                    drawn.rect = box_.response.rect;
                });
            });
        },
    );
    // epaint 0.37 asserts on a `FullOutput` dropped with texture deltas still in it; a real
    // backend uploads them and there is no backend here.
    output.drop_without_applying_deltas();
    drawn
}

/// A pointer move and then a press or a release: the move first, so the widget under the
/// point is the one that takes the press.
fn pointer_events(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// Press and release at one point, then let the page draw the frame that carries the answer.
///
/// The panel sees the click on the release frame and answers at once; the canvas answers on
/// the frame after it, because the shell draws the canvas first. Both are returned, and both
/// are asserted equal at the call sites: one answer, two doors.
fn click(ctx: &egui::Context, at: Pos2, view: &mut GraphView) -> (Option<i64>, Option<i64>) {
    let pressed = frame(ctx, pointer_events(at, true), view);
    assert_eq!(
        (pressed.canvas, pressed.panel),
        (None, None),
        "a press alone opened a story at {at:?}"
    );
    let on_click = frame(ctx, pointer_events(at, false), view).panel;
    let after = frame(ctx, Vec::new(), view).canvas;
    (on_click, after)
}

// --------------------------------------------------------------------------- the query

/// The row's stories are the stories that mention both ends — newest first, from either
/// end, cut to the limit, and never the neighbours that name only one.
#[test]
fn a_relations_stories_are_the_ones_that_name_both_ends() {
    let path = temp_archive("query");
    seed(&path);

    let stories = stories_for(&path, 1, 2, 5).expect("a readable archive");
    let ids: Vec<i64> = stories.iter().map(|story| story.id).collect();
    assert_eq!(
        ids,
        vec![10, 11, 12, 17],
        "the newest stories that name both ends, newest first"
    );
    // The neighbours: one end only (13, 14), a duplicate (15), and a page the model did not
    // call news (16) — 15 and 16 are newer than every answer, so their absence is a filter
    // and not an accident of the ordering.
    for excluded in [13, 14, 15, 16] {
        assert!(
            !ids.contains(&excluded),
            "story {excluded} does not name both ends, or is not a story the page shows"
        );
    }
    assert_eq!(
        stories[0].title, "Ships and engines",
        "the row carries the headline the reading pane will open"
    );
    assert_eq!(stories[0].source, "The Engineer", "and who filed it");

    // The same question from the other end: a relation is not directional.
    let from_other_end = stories_for(&path, 2, 1, 5).expect("a readable archive");
    assert_eq!(
        from_other_end.iter().map(|story| story.id).collect::<Vec<i64>>(),
        ids,
        "the stories of a pair do not depend on which end asked"
    );

    // The limit is the limit, and it takes the newest, not an arbitrary few.
    let few = stories_for(&path, 1, 2, 2).expect("a readable archive");
    assert_eq!(
        few.iter().map(|story| story.id).collect::<Vec<i64>>(),
        vec![10, 11]
    );
    assert!(
        stories_for(&path, 1, 2, 0).expect("a readable archive").is_empty(),
        "a limit of zero is no stories, not every story"
    );

    // A pair nothing names: empty, which the panel states as its own sentence.
    let unrelated = stories_for(&path, 1, 99, 5).expect("a readable archive");
    assert!(
        unrelated.is_empty(),
        "entity 99 mentions nothing, so no story can name both ends"
    );
}

/// A missing archive is a sentence, not an empty list, and the sentence never carries the
/// absolute path it was opened at.
#[test]
fn a_missing_archive_is_a_sentence_and_not_an_empty_relation() {
    let problem = match stories_for(Path::new("/nonexistent/atlas-no-such-archive.db"), 1, 2, 5) {
        Err(problem) => problem,
        Ok(stories) => panic!("a missing archive answered {} stories", stories.len()),
    };
    assert!(
        !problem.contains('/'),
        "the problem names a file, not a home directory: {problem:?}"
    );
    assert!(
        !problem.is_empty(),
        "an empty problem would be an unreadable row with no reason"
    );
}

// --------------------------------------------------------------------------- the panel

/// Every row of the panel is a door: the relation row answers its pair, and each story under
/// it answers its own id, in the order the rows are drawn.
#[test]
fn the_relation_panel_opens_its_stories_newest_first() {
    let path = temp_archive("panel");
    seed(&path);
    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);

    let mut view = GraphView::default().with_archive(&path);
    view.fill(entities(), edges());
    // The reader has picked the entity on the plate; the panel is what is drawn from here.
    view.selected = Some(0);
    let first = frame(&ctx, Vec::new(), &mut view);
    assert_eq!(
        (first.canvas, first.panel),
        (None, None),
        "a frame with no click opened something"
    );
    assert_eq!(view.focused(), None, "no relation is open before a click");
    assert!(
        first.rect.width() > 100.0 && first.rect.height() > 60.0,
        "the panel drew no box to aim at: {:?}",
        first.rect
    );

    // Walk the panel from its top down, one measured box at a time. The click that lands on
    // the relation row is the one that answers the pair; a click above it is a header, and a
    // click below it is one of the stories the row just opened.
    let x = first.rect.left() + 30.0;
    let mut opened: Vec<(f32, i64)> = Vec::new();
    let mut row_at: Option<f32> = None;
    let mut y = 0.0;
    while y < 420.0 {
        let (on_click, after) = click(&ctx, Pos2::new(x, first.rect.top() + y), &mut view);
        assert_eq!(
            on_click, after,
            "the panel and the canvas answered differently for the click at {y}"
        );
        if row_at.is_none() && view.focused().is_some() {
            row_at = Some(y);
        }
        if let Some(id) = on_click {
            opened.push((y, id));
        }
        y += 3.0;
    }

    let row_at = row_at.expect("no click between y=0 and y=420 opened the relation row");
    assert_eq!(
        view.focused(),
        Some((0, 1)),
        "the row that opened is the pair the one relation joins"
    );

    // The stories, in the order the rows are drawn: the newest first, as the query orders
    // them. A duplicate is not there, a one-end story is not there.
    let mut in_order: Vec<(f32, i64)> = Vec::new();
    for (at, id) in opened {
        if !in_order.iter().any(|(_, seen)| *seen == id) {
            in_order.push((at, id));
        }
    }
    let ids: Vec<i64> = in_order.iter().map(|(_, id)| *id).collect();
    assert_eq!(
        ids,
        vec![10, 11, 12, 17],
        "the panel offered {ids:?}, and every one of them is a door"
    );
    assert!(
        in_order[0].0 > row_at,
        "the stories sit under the relation row ({:?} above {:?}) — the panel reads in the \
         order a reader scans it",
        in_order[0].0,
        row_at
    );

    // The opened relation survives the frames after the click: the plate keeps ringing the
    // pair the reader is looking at, and one reader's click does not silently fall back to
    // "nothing selected".
    let later = frame(&ctx, Vec::new(), &mut view);
    assert_eq!(later.canvas, None, "a story is answered once, not every frame");
    assert_eq!(view.focused(), Some((0, 1)));
}

/// A click on the plate's own header text — above the relation row — opens nothing: the
/// panel's words are labels, not doors, and a reader who aims at them must not switch pages.
#[test]
fn the_panels_header_is_not_a_door() {
    let path = temp_archive("header");
    seed(&path);
    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);

    let mut view = GraphView::default().with_archive(&path);
    view.fill(entities(), edges());
    view.selected = Some(0);

    // The panel's own box, then the first few rows of it: the entity's name and its type and
    // mention count, which are labels.
    let box_ = frame(&ctx, Vec::new(), &mut view).rect;
    let x = box_.left() + 30.0;
    for dy in [2.0, 10.0, 18.0] {
        let (on_click, after) = click(&ctx, Pos2::new(x, box_.top() + dy), &mut view);
        assert_eq!(
            (on_click, after),
            (None, None),
            "the header at {dy} px below the panel's top answered a story"
        );
        assert_eq!(
            view.focused(),
            None,
            "the header at {dy} px below the panel's top opened a relation"
        );
    }
}
