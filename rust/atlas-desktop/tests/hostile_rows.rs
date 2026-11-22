//! Hostile rows: the window's own half of a database it does not write.
//!
//! `data/atlas.db` is written by the collection side and read by this window. A row in
//! it is therefore *input*, not fact: a `published` stamp is whatever bytes a scraper
//! stored, a title can be as long as a page, a cadence can be `i64::MAX`. Each of the
//! tests here pins one thing the window must do with such a row, and the measured
//! numbers came from this file — the frames are drawn by `egui::Context::run_ui` with no
//! window, no GPU surface and no driver, in the same container stack `app.rs` puts a
//! page in, so the cost asserted on is the cost a reader would feel.
//!
//! Every claim below is a regression that was measured first:
//!
//! * `views::when` used to slice `&day[8..10]` after only checking that byte 10 happened
//!   to be a boundary, so `2026-11€T10:25:12Z` panicked — and a panic unwinds through the
//!   window's event callback, so the window closed. It then echoed a stamp it could not
//!   read back whole: a 20 000-character `published` became a 20 000-character time
//!   label, 298 rows tall. The shape of the fix is the integrator's; the pins are here.
//! * The Overview's rows shaped the whole string into a one-line layout job, so 400 rows
//!   of a 100 000-character title cost 135–145 ms per frame against 1–2 ms for 400
//!   ordinary rows (measured here, twice, before the bound). A one-line row is handed the
//!   prefix that can still be drawn now, and the difference is measured by
//!   [`four_hundred_hostile_rows_cost_a_frame_and_not_a_stall`].
//! * A cadence of `i64::MAX` minutes used to read "asked again every
//!   9223372036854775807 minutes", and a monitor whose name was whitespace read as a
//!   blank row.
//! * Every dot on the map is a door to the newest story filed at that place, and the
//!   archive is read as two queries — the dots, then the list — so a story can be gone
//!   between them. A dot can therefore carry an id no row in the list has: a click on it
//!   must answer *that* id and nothing else, and a place with no story behind it must
//!   answer nothing at all (see
//!   [`a_click_on_a_pin_whose_story_is_gone_opens_that_pin_or_nothing`]).
//!
//! Nothing here opens a socket, spawns a process, or writes: the rows are built in
//! memory as `data::Article`/`data::Monitor`/`data::Pin`, which is what a page is handed.

use std::time::{Duration, Instant};

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::data::{Article, Counts, Monitor, Pin};
use atlas_desktop::theme;
use atlas_desktop::views::monitors::{cadence_text, row_name};
use atlas_desktop::views::overview::{self, head, Overview, Snapshot};
use atlas_desktop::views::when;

/// The window's own default size (`main.rs`): a page is measured at the width it ships at.
const SCREEN: Vec2 = Vec2::new(1400.0, 900.0);

/// The longest label `views::when` may return: the 24 characters `views::clip` keeps,
/// plus the ellipsis it appends. Written as a constant so the assertion reads as a rule.
const LABEL: usize = 25;

/// The most characters one line of a row can hold at this screen width: the Overview's
/// [`atlas_desktop::views::overview::head`] is given twice the narrowest-advance estimate
/// plus sixteen, which at 1400 pixels is around a thousand. The ceiling is the bound with
/// room to spare; before the prefix was taken, one row held the whole 100 000.
const LINE_MAX: usize = 1_200;

/// The most labels a page of rows can draw at this screen height: the visible rows of a
/// 900-pixel window, three lines each, plus the page's own headings and notes.
const LABELS_MAX: usize = 90;

// ------------------------------------------------------------------------ the page

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    // The palette, the type ladder and the spacing the window installs (`Atlas::new`),
    // so the measurement is of the shipped style rather than egui's default.
    theme::install(&ctx);
    ctx
}

/// The input of one frame: a screen, the events a test sends, a clock that moves.
fn input(index: usize, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(index as f64 / 60.0),
        events,
        ..Default::default()
    }
}

/// One frame, with the frame's `TexturesDelta` released before it is dropped.
///
/// `epaint 0.37` panics on `Drop` when a `TexturesDelta` still carries unapplied deltas
/// (`epaint-0.37.0/src/textures.rs:337`, "Deltas need to be handled"). A real backend
/// uploads them; there is no renderer here, and the measurements below read `shapes`, which
/// are complete without a single texture uploaded. So the deltas are cleared explicitly —
/// the handling that crate asks for — instead of arriving at `Drop` and panicking, which
/// would make every case fail for a reason that has nothing to do with the row.
fn frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    mut draw: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, |ui| draw(ui));
    output.textures_delta.clear();
    output
}

/// What one frame drew, and what it cost to draw it.
#[derive(Default)]
struct Painted {
    /// Wall time of the last frame, in milliseconds: layout, paint and the shapes.
    ms: f64,
    /// The characters in the longest single layout job — the work one label asked for.
    longest: usize,
    /// Every character that reached a layout job, summed over the frame.
    chars: usize,
    /// Every glyph drawn, summed over the frame.
    glyphs: usize,
    /// How many separate strings became galleys.
    texts: usize,
    /// Every string that reached a galley, one after another: what the page *said*.
    text: String,
    /// Every circle drawn, by centre and radius: where the map put its dots, so a test can
    /// click the one it just measured instead of guessing at a projection.
    circles: Vec<(Pos2, f32)>,
}

/// Run `frames` frames of `page` and keep the last one's cost.
///
/// The containers are `app.rs`'s own: a `CentralPanel` with the page's inner margin and a
/// vertical `ScrollArea` that never shrinks. A page that only misbehaves inside a scroll
/// area therefore misbehaves here.
fn paint(ctx: &egui::Context, frames: usize, mut page: impl FnMut(&mut egui::Ui)) -> Painted {
    paint_with(ctx, 0, frames, Vec::new(), &mut page)
}

// ------------------------------------------------------- the frame's cost, measured

/// How many samples a timing takes before it is anyone's fault but the machine's.
const SAMPLES: usize = 5;

/// The wall clock the search for a cheap sample may spend on one side of a comparison.
///
/// Five samples of a two-millisecond frame are ten milliseconds of work, so a scheduler
/// that deschedules this process for a tenth of a second can swallow *all* of them. Given
/// this much sampling, some slice of it is clean.
const BUDGET: Duration = Duration::from_millis(400);

/// The most one frame of this page may cost in a debug build, in milliseconds.
///
/// This is the second gate on [`four_hundred_hostile_rows_cost_a_frame_and_not_a_stall`]:
/// the ratio against an ordinary page says *the bound is still doing its job*, and this
/// number says the page is a frame at all. Measured here (debug, best of five, over several
/// runs on a machine that was also compiling other crates): 3.2–5.7 ms for 400 hostile rows
/// against 2.6–5.5 ms for ordinary ones — the bound had already done its work, so the two
/// pages cost the same. The ceiling is ten times the worst of those, because a debug build on
/// a busy machine is what the sampling exists to survive, and it stays far below the 135–145 ms
/// per frame the unbounded row cost, which is the regression it guards.
const HOSTILE_CEILING: f64 = 50.0;

/// The cheapest of several frames of `page`, and how many samples it took.
///
/// A frame's cost is not the machine's load. In a full `cargo test` run other targets are
/// laying out pages on the same cores, and a working renderer caught on a descheduled
/// slice reads as a slow one; the number compared below must be the *minimum* of several
/// frames, not whatever the last sample happened to catch. So: take at least [`SAMPLES`]
/// samples, and keep taking them while the best is still over `comfortable` (the caller
/// passes a number the assertion cannot fire on) and [`BUDGET`] of wall clock is left.
///
/// The page's own state is rebuilt per sample — `Overview::new()` every time — so the
/// measurement is of the work a reader's frame does, not of a view that has already
/// cached the page. What this cannot hide is a regression: a page that shapes a title per
/// row is slow in *every* sample, so the best of them is still over the ceiling.
fn cheapest_paint(ctx: &egui::Context, page: &Snapshot, comfortable: f64) -> (Painted, usize) {
    let started = Instant::now();
    let mut best: Option<Painted> = None;
    let mut taken = 0usize;
    loop {
        let mut view = Overview::new();
        let painted = paint(ctx, 1, |ui| {
            let _ = overview::show(ui, &mut view, page);
        });
        taken += 1;
        if best.as_ref().map(|kept| painted.ms < kept.ms).unwrap_or(true) {
            best = Some(painted);
        }
        let spent = best.as_ref().map(|kept| kept.ms).unwrap_or(f64::INFINITY);
        if taken >= SAMPLES && (spent <= comfortable || started.elapsed() >= BUDGET) {
            break;
        }
    }
    (best.expect("at least one sample is always taken"), taken)
}

/// The same, with the same events sent on every frame: how a test clicks the dot it
/// measured on the frame before.
fn paint_with(
    ctx: &egui::Context,
    first: usize,
    frames: usize,
    events: Vec<egui::Event>,
    page: &mut impl FnMut(&mut egui::Ui),
) -> Painted {
    let mut painted = Painted::default();
    for index in first..first + frames {
        let started = Instant::now();
        // `egui 0.37` hands the frame a root `Ui` (`Context::run_ui`; `Context::run` is
        // gone) and a panel is shown inside it — the shape `app::Atlas::draw` uses.
        let output = frame(ctx, input(index, events.clone()), |ui| {
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
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        painted = Painted { ms, ..measure(&output.shapes) };
    }
    painted
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
            let text_chars = text.galley.text().chars().count();
            painted.chars += text_chars;
            painted.longest = painted.longest.max(text_chars);
            painted.texts += 1;
            painted.text.push_str(text.galley.text());
            painted.text.push('\n');
            for placed in &text.galley.rows {
                painted.glyphs += placed.row.glyphs.len();
            }
        }
        Shape::Circle(circle) => painted.circles.push((circle.center, circle.radius)),
        _ => {}
    }
}

// ------------------------------------------------------------------- the stories

fn story(id: i64, title: &str, body: &str, published: &str) -> Article {
    Article {
        id,
        title: title.to_string(),
        source: "Reuters".to_string(),
        published: published.to_string(),
        summary: String::new(),
        content: body.to_string(),
        country: "US".to_string(),
        url: "https://example.test/a".to_string(),
        full_text: true,
    }
}

/// The Overview's input: the same shape `app::Atlas::reload` builds, with the map left
/// out (no pins) so the frame under measurement is the story list and nothing else.
fn snapshot(articles: Vec<Article>) -> Snapshot {
    Snapshot {
        pins: Vec::new(),
        countries: Vec::new(),
        counts: Counts { relevant: articles.len() as i64, ..Counts::default() },
        articles,
        hours: 72,
    }
}

// ------------------------------------------------------------------------ the pins

/// A stamp this window cannot read is a label, not a program.
///
/// The old `when` sliced `&day[8..10]` after checking only that byte 10 and byte 16 were
/// boundaries, so a multi-byte character sitting where the day or the time sits panicked
/// while slicing — measured: `2026-11€T10:25:12Z` →
/// `start byte index 8 is not a char boundary` — and the panic closed the window. It then
/// returned an unreadable stamp *whole*, so a 20 000-character `published` became a
/// 20 000-character label and a 298-row-tall story.
#[test]
fn a_stamp_that_is_not_a_stamp_is_a_bounded_label_and_never_a_panic() {
    // An empty stamp is an empty label: a row with no date has no date to show.
    assert_eq!(when(""), "");

    // A valid stamp still reads as a day, which is the thing the bound must not break.
    assert_eq!(when("1998-03-04T09:05:00Z"), "04 Mar");
    assert_eq!(when("1998-12-31T23:59:59Z"), "31 Dec");

    let hostile = [
        // a euro sign where the day sits: bytes 8 and 9 are inside the character
        "2026-11\u{20AC}T10:25:12Z".to_string(),
        // the same for the month: byte 6 is inside the character
        "2026-1\u{20AC}1-21T10:25:12Z".to_string(),
        // a right-to-left override where `T` sits
        "1998-03-04\u{202E}09:05:00Z".to_string(),
        // nothing but multi-byte characters, long enough to flood any echo
        "\u{20AC}".repeat(20_000),
        // twenty thousand digits: the right shape and length, no separators
        "9".repeat(20_000),
        // a real stamp with a hostile tail
        format!("1998-03-04T09:05:00Z{}", "\u{20AC}".repeat(20_000)),
    ];
    for stamp in &hostile {
        let label = when(stamp);
        assert!(
            label.chars().count() <= LABEL,
            "a {} -character stamp came back as a {}-character label: {:?}",
            stamp.chars().count(),
            label.chars().count(),
            label
        );
        assert!(!label.is_empty(), "an unreadable stamp still gets a label: {stamp:?}");
    }

    // The bound is on *characters*, not bytes: a stamp of euro signs must not be cut
    // through the middle of one either.
    let label = when(&"\u{20AC}".repeat(20_000));
    assert!(label.ends_with('\u{2026}'), "the cut is marked: {label:?}");
    assert_eq!(head(&label, LABEL), label, "the label fits inside the bound: {label:?}");
}

/// The prefix a one-line row is handed is bounded and cut on a character boundary.
///
/// This is the unit the Overview's rows are built from: `head` is what stops a
/// 100 000-character title from becoming a 100 000-glyph layout job, and it must never
/// cut a multi-byte character in half on the way.
#[test]
fn the_prefix_handed_to_a_line_is_bounded_and_cut_on_a_boundary() {
    let title = "T".repeat(100_000);
    assert_eq!(head(&title, 480).chars().count(), 480);
    assert_eq!(head(&title, 480).len(), 480, "ASCII: 480 characters are 480 bytes");
    // a multi-byte string: three characters are three characters, nine bytes
    assert_eq!(head(&"\u{20AC}".repeat(1_000), 3), "\u{20AC}\u{20AC}\u{20AC}");
    // a short string is returned whole, not padded
    assert_eq!(head("A headline", 480), "A headline");
    assert_eq!(head("A headline", 10), "A headline", "the cap is a maximum, not a pad");
    // the empty string is a prefix of itself
    assert_eq!(head("", 0), "");
}

/// Four hundred hostile rows cost a frame, not a stall.
///
/// The Overview's row is one elided line, but `max_rows: 1` does not bound the work:
/// `epaint::text::layout` shapes the whole string and drops the rows past the first, so
/// every row used to shape its entire title and its entire body. Measured before the
/// bound: 242 ms per frame for 400 such rows, 0.5 ms for one. The assertions are the
/// bound itself (no layout job may hold more than a line's worth of text), the shape of
/// the cost (a page of hostile rows costs about what a page of ordinary rows costs), and
/// a ceiling on the frame ([`HOSTILE_CEILING`]).
///
/// Both numbers compared here are the *cheapest* of several frames, not the last one
/// caught: see [`cheapest_paint`] and [`SAMPLES`]. A wall clock is the one measurement a
/// test cannot take on a machine it shares, and the failing version of this assertion was
/// not a slow renderer — it was a working one, descheduled mid-frame by the other targets
/// of a full `cargo test` run.
#[test]
fn four_hundred_hostile_rows_cost_a_frame_and_not_a_stall() {
    let hostile_stamp = "2026-11\u{20AC}T10:25:12Z";
    let title = "T".repeat(100_000);
    let body = "line\n".repeat(10_000);

    let ordinary: Vec<Article> = (0..400)
        .map(|id| story(id, "A headline of sixty characters, about a chip maker's quarter", "A paragraph of body text.", "1998-03-04T09:05:00Z"))
        .collect();
    let attack: Vec<Article> = (0..400)
        .map(|id| story(id, &title, &body, hostile_stamp))
        .collect();

    let ctx = context();
    let ordinary_page = snapshot(ordinary);
    let attack_page = snapshot(attack);
    // `comfortable` is half a ceiling, never the ceiling itself: the loop keeps sampling
    // only while the best frame is still over a number the assertion cannot fire on.
    let (baseline, baseline_samples) = cheapest_paint(&ctx, &ordinary_page, HOSTILE_CEILING / 8.0);
    let (hostile, hostile_samples) = cheapest_paint(&ctx, &attack_page, HOSTILE_CEILING / 2.0);

    println!(
        "overview, 400 rows: ordinary {:.1} ms/frame (best of {baseline_samples}, {} chars shaped, {} glyphs) · hostile {:.1} ms/frame (best of {hostile_samples}, {} chars shaped, {} glyphs, longest job {} chars)",
        baseline.ms, baseline.chars, baseline.glyphs, hostile.ms, hostile.chars, hostile.glyphs, hostile.longest
    );

    // The structural bound: a row is one elided line, so no single layout job may hold a
    // whole hostile title. Before the bound this was 100 000 characters per row.
    assert!(
        hostile.longest <= LINE_MAX,
        "one row's layout job held {} characters: the prefix is not bounded",
        hostile.longest
    );
    // ...and the page shapes a line per row, not a title per row: at most the labels that
    // fit on the screen. Before the bound this frame shaped 1 350 475 characters.
    assert!(
        hostile.chars <= LINE_MAX * LABELS_MAX,
        "{} characters were shaped for one page of rows",
        hostile.chars
    );
    // The ceiling: the cheapest of many frames is what a debug build may spend, so a
    // stolen slice of the machine is not a dropped frame, and a page that went back to
    // shaping whole titles is over the line in every sample it can take.
    assert!(
        hostile.ms <= HOSTILE_CEILING,
        "one frame of 400 hostile rows cost {:.1} ms (best of {hostile_samples}), over the \
         {HOSTILE_CEILING} ms ceiling a debug build is allowed",
        hostile.ms
    );
    // The cost: within a small multiple of the same page with ordinary rows. The bound is
    // loose on purpose — this is a wall clock in a debug build, on a machine that is also
    // running other builds — but 50x (the measured ratio before the bound) is not close.
    assert!(
        hostile.ms <= baseline.ms * 4.0 + 12.0,
        "400 hostile rows cost {:.1} ms (best of {hostile_samples}) against {:.1} ms for \
         ordinary ones (best of {baseline_samples})",
        hostile.ms,
        baseline.ms
    );
}

/// A hostile stamp reaching the *page* — not just the function — draws and does not
/// panic, which is the difference between a bad label and a closed window.
#[test]
fn a_page_of_unreadable_stamps_draws() {
    let flood = "\u{20AC}".repeat(2_000);
    let stamps = ["2026-11\u{20AC}T10:25:12Z", flood.as_str(), "", "1998-03-04T09:05:00Z"];
    let articles: Vec<Article> = (0..40)
        .map(|id| story(id, "A normal headline", "A normal body.", stamps[id as usize % stamps.len()]))
        .collect();
    let ctx = context();
    let mut view = Overview::new();
    let page = snapshot(articles);
    let painted = paint(&ctx, 2, |ui| {
        let _ = overview::show(ui, &mut view, &page);
    });
    assert!(painted.texts > 0, "the page drew nothing at all");
    // No label held the flood stamp: the longest string on the page is one of the page's
    // own sentences (the empty-map note is ~110 characters), not a 2 000-character stamp.
    assert!(
        !painted.text.contains(&"\u{20AC}".repeat(LABEL)),
        "a stamp reached a label whole"
    );
    assert!(painted.longest <= 200, "a label held {} characters", painted.longest);
}

/// The bound changes nothing about a title the window can hold.
///
/// This is the other half of the contract: a row is one elided line, and a title short
/// enough to be drawn is handed to the layout engine character for character — the
/// prefix only ever cuts text that could not have appeared on the line.
#[test]
fn an_ordinary_title_reaches_the_galley_character_for_character() {
    let ordinary = "A headline of sixty characters, about a chip maker's quarter";
    let longer = concat!(
        "A headline that is long enough to be elided at any window width the app opens at, ",
        "and still short enough that every character of it is worth laying out: the point ",
        "of the bound is to cut what cannot be drawn, not to cut what can."
    );
    for title in [ordinary, longer] {
        assert!(title.chars().count() < 500, "the fixture is inside the prefix: {}", title.len());
        let ctx = context();
        let mut view = Overview::new();
        let page = snapshot(vec![story(1, title, "A body.", "1998-03-04T09:05:00Z")]);
        let painted = paint(&ctx, 2, |ui| {
            let _ = overview::show(ui, &mut view, &page);
        });
        assert!(
            painted.text.lines().any(|line| line == title),
            "a {}-character title did not reach the galley whole: {:?}",
            title.chars().count(),
            painted.text
        );
    }

    // And the hostile one is not drawn whole, which is the whole point.
    let hostile = "T".repeat(100_000);
    let ctx = context();
    let mut view = Overview::new();
    let page = snapshot(vec![story(1, &hostile, "A body.", "1998-03-04T09:05:00Z")]);
    let painted = paint(&ctx, 2, |ui| {
        let _ = overview::show(ui, &mut view, &page);
    });
    assert!(
        !painted.text.lines().any(|line| line == hostile),
        "the whole 100 000-character title reached a galley"
    );
    assert!(painted.longest <= LINE_MAX, "longest job {} chars", painted.longest);
}

// ------------------------------------------------------------- the map's click

/// The page's input with the map's dots on it: the pins the map draws, and the stories the
/// list under it holds.
fn snapshot_with(pins: Vec<Pin>, articles: Vec<Article>) -> Snapshot {
    Snapshot { pins, ..snapshot(articles) }
}

/// Where the page drew its one dot, read out of the frame.
///
/// Not computed from a projection: a test that works out the plate's corner itself is
/// measuring its own arithmetic, and a dot it "clicks" is a dot the page never drew. The
/// shapes say where the dot is.
fn dot_of(ctx: &egui::Context, view: &mut Overview, page: &Snapshot) -> Pos2 {
    let painted = paint(ctx, 3, |ui| {
        let _ = overview::show(ui, view, page);
    });
    assert_eq!(
        painted.circles.len(),
        1,
        "the page drew {} circles, so which one is the dot is a guess: {:?}",
        painted.circles.len(),
        painted.circles
    );
    painted.circles[0].0
}

/// One click at `at`: a press frame and then a release frame, which is where egui reports
/// `clicked()`. Answers the story the page opened, which is what the window would switch to.
fn click_at(ctx: &egui::Context, view: &mut Overview, page: &Snapshot, at: Pos2) -> Option<i64> {
    let button = |pressed: bool| {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ]
    };
    paint_with(ctx, 3, 1, button(true), &mut |ui| {
        let _ = overview::show(ui, view, page);
    });
    let mut opened = None;
    paint_with(ctx, 4, 1, button(false), &mut |ui| {
        opened = overview::show(ui, view, page);
    });
    opened
}

/// A dot whose story is gone answers that id, and a dot with no story answers nothing.
///
/// The map and the list under it are two queries against an archive this window does not
/// write, so the newest story at a place can be deleted between them: the dot then carries
/// an id that no row of `Snapshot::articles` has. Two things must not happen when a reader
/// clicks it — a panic while the id is looked up, which unwinds through the window's event
/// callback and closes it, and a *different* story opening, which would tell a reader
/// something the archive does not say.
#[test]
fn a_click_on_a_pin_whose_story_is_gone_opens_that_pin_or_nothing() {
    let articles = vec![
        story(11, "A chip maker's quarter", "A body.", "1998-03-04T09:05:00Z"),
        story(12, "Something filed elsewhere", "A body.", "1998-03-05T09:05:00Z"),
    ];
    // One page: one dot at Kinshasa carrying `article`, the same two stories under it. The
    // list is a real one, so "another story opened" is observable rather than vacuous.
    let fixture = |article: i64| {
        let pins = vec![Pin {
            place: "Kinshasa".to_string(),
            lat: -4.3,
            lon: 15.3,
            count: 3,
            article,
        }];
        snapshot_with(pins, articles.clone())
    };
    // Click the dot the page drew, and answer the story the page opened.
    let click = |page: &Snapshot| {
        let ctx = context();
        let mut view = Overview::new();
        let dot = dot_of(&ctx, &mut view, &page);
        (dot, click_at(&ctx, &mut view, page, dot))
    };

    // The id the dot carries is not among the stories the page holds — deleted between the
    // two queries, past the end of the archive, or an ordinary small id whose row is gone.
    // The page answers that id and nothing else: the reader is told "that story is not
    // here" instead of being shown somebody else's news.
    for stale in [9_999_999, i64::MAX, 1] {
        let page = fixture(stale);
        let (dot, opened) = click(&page);
        assert_eq!(
            opened,
            Some(stale),
            "a click on the dot at {dot:?} answered {opened:?}, not the id the dot carries"
        );
        for article in &page.articles {
            assert_ne!(
                opened,
                Some(article.id),
                "the click opened story {}, which the dot does not point at",
                article.id
            );
        }
    }

    // A dot that missed every place opens nothing: 60 points from a dot of radius three is
    // the water beside it.
    let page = fixture(9_999_999);
    let (dot, _) = click(&page);
    let beside = dot + Vec2::new(60.0, 60.0);
    assert_eq!(
        click_at(&context(), &mut Overview::new(), &page, beside),
        None,
        "a click at {beside:?}, 60 points from the nearest dot, opened a story"
    );

    // And a place the archive located but could not file a story for — `views::overview`
    // screens `pin.article > 0` — must open nothing rather than the first row of the list.
    for absent in [0, -1, i64::MIN] {
        let page = fixture(absent);
        let (_, opened) = click(&page);
        assert_eq!(
            opened, None,
            "a dot with article {absent} opened {opened:?}: a place with no story is not a story"
        );
    }
}

// ---------------------------------------------------------------------- the monitors

/// A cadence the archive stores is a number this window has to say out loud, and
/// `i64::MAX` minutes is not a schedule a person can wait for.
#[test]
fn a_cadence_past_a_year_reads_as_something_a_person_can_use() {
    // The sentences that were already right, unchanged.
    assert_eq!(cadence_text(0), "asked only when you ask it");
    assert_eq!(cadence_text(-5), "asked only when you ask it");
    assert_eq!(cadence_text(60), "asked again every hour");
    assert_eq!(cadence_text(240), "asked again every 4 hours");
    assert_eq!(cadence_text(90), "asked again every 90 minutes");

    // The hostile end: measured before the clamp as
    // "asked again every 9223372036854775807 minutes".
    for absurd in [i64::MAX, i64::MAX - 1, 100 * 365 * 24 * 60, 525_601] {
        let sentence = cadence_text(absurd);
        assert!(
            !sentence.contains(&absurd.to_string()) && !sentence.contains("9223372036854775807"),
            "{absurd} minutes came back as {sentence:?}"
        );
        assert!(sentence.chars().count() < 60, "{absurd}: {sentence:?}");
        assert!(sentence.starts_with("asked again"), "{absurd}: {sentence:?}");
    }
    // A year and a day is still a cadence: the clamp is at the far end, not at a week.
    assert_eq!(cadence_text(365 * 24 * 60), "asked again every 8 760 hours");
    assert_eq!(cadence_text(1_440), "asked again every 24 hours");
}

/// A monitor whose name is nothing but whitespace still gets a row a reader can name.
#[test]
fn a_monitor_with_a_blank_name_is_named_by_its_id() {
    let blank = Monitor { id: 7, name: "   ".to_string(), ..Monitor::default() };
    assert_eq!(row_name(&blank), "monitor 7");
    let empty = Monitor { id: 12, name: String::new(), ..Monitor::default() };
    assert_eq!(row_name(&empty), "monitor 12");
    // a real name is left exactly as the archive stored it
    let named = Monitor { id: 3, name: "Chip export rules".to_string(), ..Monitor::default() };
    assert_eq!(row_name(&named), "Chip export rules");
}
