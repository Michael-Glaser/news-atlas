//! The Overview page: every door on it, clicked; the page's own box, measured.
//!
//! The page is drawn alone — no rail, no status row — so the box this file asks
//! `Context::run_ui` for **is** the page box, and "inside the page" is a rectangle this
//! test can compare against. Six claims, in the order a reader would meet them:
//!
//! 1. **The plate fits the box at every size the layout was audited at** (700×520,
//!    900×620, 1280×820, 2400×1400), grows with the window, and stops at the ceiling.
//!    Measured before the change: at 700×520 the plate was 470 px tall while 328 px fit,
//!    and its own caption was painted at y = 586 in a 520 px window.
//! 2. **The page is the sum of its cards**, never a typed height: the stories card ends
//!    where the box ends, the gap between the two cards is the same at every size, and
//!    nothing is painted below the box.
//! 3. **The stories column scrolls inside itself**: a wheel over it moves the rows and
//!    leaves the map exactly where it was.
//! 4. **A click on a place chip answers that place and never a story**, and a click on a
//!    story row answers its id — the two doors of the page, kept apart.
//! 5. **A click on a dot on the plate still opens that place's newest story.** This is the
//!    map path `tests/e2e_map_click.rs` proves through the whole window; it is proved here
//!    as well, against the page, because the plate's height is no longer a constant and a
//!    test that finds the plate by "470 px tall" would have stopped measuring anything.
//! 6. **Hostile place names do not decide the page's width**, and the three empty states
//!    still say what would fill them.
//!
//! Nothing here re-implements the page's arithmetic: the plate is the rectangle the map
//! clears to the page colour, the cards are the panel-coloured rectangles, a widget is
//! found by the id the page gives it (`overview::place_id`, `overview::story_id`) through
//! `Context::read_response`, and a click is aimed at that widget's own rect.

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::data::{Article, Counts, Pin};
use atlas_desktop::theme;
use atlas_desktop::views::overview::{self, Choice, Door, Kind, Overview, Snapshot};

// --------------------------------------------------------------------------- fixtures

fn article(id: i64, title: &str) -> Article {
    Article {
        id,
        title: title.to_string(),
        published: "2026-11-21T10:25:12Z".to_string(),
        summary: "The words the archive kept.".to_string(),
        content: "The body of the story.".to_string(),
        ..Article::default()
    }
}

fn pin(place: &str, lat: f64, lon: f64, article: i64) -> Pin {
    Pin {
        place: place.to_string(),
        lat,
        lon,
        count: 1,
        article,
    }
}

fn snapshot(
    articles: Vec<Article>,
    countries: Vec<(&str, i64)>,
    pins: Vec<Pin>,
) -> Snapshot {
    Snapshot {
        pins,
        countries: countries
            .into_iter()
            .map(|(place, count)| (place.to_string(), count))
            .collect(),
        articles,
        counts: Counts::default(),
        hours: 72,
    }
}

/// `n` stories, each with a title that says which row it is.
fn stories(n: usize) -> Vec<Article> {
    (1..=n)
        .map(|index| article(index as i64, &format!("Story {index} about the harbour")))
        .collect()
}

// ------------------------------------------------------------------ the page, in a box

#[derive(Default)]
struct Painted {
    /// Every filled rectangle, as painted inside its scissor: `(visible rect, fill)`.
    fills: Vec<(Rect, egui::Color32)>,
    /// Filled circles that are on screen — the dots on the plate (a stroked one is a ring).
    dots: Vec<(Pos2, f32)>,
    /// Every galley, with the part of it that was not clipped away.
    texts: Vec<(String, Rect)>,
    /// Stroked rectangles — the focus rings — with the colour they were drawn in.
    rings: Vec<(Rect, egui::Color32)>,
    /// Every galley as it was shaped, with the scissor it was drawn under: `(text,
    /// galley rect, clip)`. Kept un-intersected because a run cut by its scissor looks
    /// exactly like a run that fits, and the difference is what one of the defects was.
    runs: Vec<(String, Rect, Rect)>,
}

impl Painted {
    fn text(&self) -> String {
        self.texts
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The plate: the largest rectangle the map clears to the page colour.
    fn plate(&self) -> Rect {
        self.fills
            .iter()
            .filter(|(_, fill)| *fill == theme::PAGE)
            .map(|(rect, _)| *rect)
            .max_by(|left, right| {
                (left.width() * left.height())
                    .partial_cmp(&(right.width() * right.height()))
                    .expect("two finite areas")
            })
            .unwrap_or_else(|| panic!("no page-coloured rectangle was painted: {:?}", self.fills))
    }

    /// The two cards, top to bottom.
    fn cards(&self) -> Vec<Rect> {
        let mut cards: Vec<Rect> = self
            .fills
            .iter()
            .filter(|(_, fill)| *fill == theme::PANEL)
            .map(|(rect, _)| *rect)
            .collect();
        cards.sort_by(|left, right| {
            left.top()
                .partial_cmp(&right.top())
                .expect("two finite rows")
        });
        cards
    }

    /// The plate's own caption ("zoom ×1.0 · …"), with the box it was drawn in.
    fn caption(&self) -> Rect {
        self.texts
            .iter()
            .find(|(text, _)| text.starts_with("zoom ×"))
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("the plate painted no caption:\n{}", self.text()))
    }
}

struct Page {
    ctx: egui::Context,
    view: Overview,
    data: Snapshot,
    size: Vec2,
}

impl Page {
    fn new(size: Vec2, data: Snapshot) -> Page {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        Page {
            ctx,
            view: Overview::new(),
            data,
            size,
        }
    }

    fn input(&self, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            events,
            ..Default::default()
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> (Choice, Painted) {
        let mut choice = Choice::default();
        let mut output = self.ctx.run_ui(self.input(events), |ui| {
            choice = overview::show_choices(ui, &mut self.view, &self.data);
        });
        // No backend here: the deltas are the renderer's business, and a window a hundred
        // frames tall in a test must not keep them.
        output.textures_delta.clear();
        (choice, record(&output.shapes))
    }

    /// Three frames with no input, and the last one: the plate frames its camera on the
    /// first frame it draws and paints its dots from the next, so "settled" is what a test
    /// must measure rather than the init frame.
    fn quiet(&mut self) -> (Choice, Painted) {
        let mut settled = self.frame(Vec::new());
        for _ in 0..2 {
            settled = self.frame(Vec::new());
        }
        settled
    }

    /// Press and release at `at`: egui reports `clicked()` on the release, so a click is
    /// two frames, and the second one is the one that answers.
    fn click(&mut self, at: Pos2) -> (Choice, Painted) {
        self.frame(vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            let (choice, painted) = self.frame(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ]);
            if !pressed {
                return (choice, painted);
            }
        }
        unreachable!("the release frame answers")
    }

    /// The rect a widget was given last frame, by the id the page keyed it with.
    fn widget(&mut self, id: egui::Id) -> Rect {
        self.quiet();
        let response = self
            .ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("the page drew no widget with id {id:?}"));
        response.rect
    }

    fn click_widget(&mut self, id: egui::Id) -> (Choice, Painted) {
        let at = self.widget(id).center();
        self.click(at)
    }

    /// A wheel over `at`, then frames until the column stops moving.
    fn wheel(&mut self, at: Pos2, delta: f32) -> (Choice, Painted) {
        self.frame(vec![egui::Event::PointerMoved(at)]);
        let (mut choice, mut painted) = self.frame(vec![
            egui::Event::PointerMoved(at),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, delta),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::default(),
            },
        ]);
        // `egui` smooths the wheel: the area keeps moving for a few frames, and a test
        // that read the first one would measure the animation rather than the scroll.
        let mut previous = painted.text();
        for _ in 0..40 {
            let (next, frame) = self.frame(vec![egui::Event::PointerMoved(at)]);
            choice = next;
            painted = frame;
            if painted.text() == previous {
                break;
            }
            previous = painted.text();
        }
        (choice, painted)
    }
}

fn record(shapes: &[egui::epaint::ClippedShape]) -> Painted {
    let mut painted = Painted::default();
    for clipped in shapes {
        walk(&clipped.shape, clipped.clip_rect, &mut painted);
    }
    painted
}

/// Every fill, dot and galley, intersected with the scissor it was drawn under: a row
/// scrolled out of the column is clipped away, and a measurement that ignored the clip
/// would count it as a page that overflows.
fn walk(shape: &Shape, clip: Rect, painted: &mut Painted) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, clip, painted);
            }
        }
        Shape::Text(text) => {
            let full = Rect::from_min_size(text.pos, text.galley.size());
            painted
                .runs
                .push((text.galley.text().to_string(), full, clip));
            let box_ = full.intersect(clip);
            if !box_.is_negative() && box_.area() > 0.0 {
                painted.texts.push((text.galley.text().to_string(), box_));
            }
        }
        Shape::Circle(circle) if circle.fill.a() > 0 => {
            if clip.contains(circle.center) {
                painted.dots.push((circle.center, circle.radius));
            }
        }
        Shape::Rect(rect) if rect.fill.a() > 0 => {
            let box_ = rect.rect.intersect(clip);
            if !box_.is_negative() && box_.area() > 0.0 {
                painted.fills.push((box_, rect.fill));
            }
        }
        Shape::Rect(rect) if rect.stroke.width > 0.0 => {
            painted.rings.push((rect.rect, rect.stroke.color));
        }
        _ => {}
    }
}

// --------------------------------------------------------------------------- 1. the box

/// The plate is inside the page box at every audited size, grows with the window, and
/// stays under its ceiling.
///
/// Measured before the change (`/tmp/atlas-audit-layout.md`): 700×520 → a plate 470 px
/// tall of which 328 px were inside the box, caption at y = 586 in a 520 px window;
/// 900×620 → 428 of 470 px visible; 2400×1400 → 2134×470, 35.3 % of a 1330 px box with
/// ~860 px of nothing under the card.
#[test]
fn the_plate_fits_the_box_at_every_size_and_grows_with_it() {
    let mut heights = Vec::new();
    // The page boxes of the audit (`/tmp/atlas-audit-layout.md`): the window less the
    // 188 px rail, its two 20 px margins, the 38 px status row and the two 16 px margins.
    for size in [
        Vec2::new(472.0, 450.0),
        Vec2::new(672.0, 550.0),
        Vec2::new(1052.0, 750.0),
        Vec2::new(1452.0, 1130.0),
        Vec2::new(2172.0, 1330.0),
    ] {
        let mut page = Page::new(
            size,
            snapshot(stories(4), vec![("SE", 9)], vec![pin("Nairobi", -1.3, 36.8, 2)]),
        );
        let (_, painted) = page.quiet();
        let box_ = Rect::from_min_size(Pos2::ZERO, size);
        let plate = painted.plate();
        assert!(
            box_.contains_rect(plate),
            "at {size:?} the plate {plate:?} is outside the page box {box_:?}"
        );
        let caption = painted.caption();
        assert!(
            box_.contains_rect(caption),
            "at {size:?} the plate's caption {caption:?} is outside the page box {box_:?}"
        );
        assert!(
            plate.height() <= overview::MAX_PLATE + 0.5,
            "at {size:?} the plate is {} (ceiling {})",
            plate.height(),
            overview::MAX_PLATE
        );
        assert!(
            plate.height() >= overview::MIN_PLATE - 0.5,
            "at {size:?} the plate is {} (floor {})",
            plate.height(),
            overview::MIN_PLATE
        );
        heights.push(plate.height());
        // The measurement this test asserts, printable on demand:
        // `cargo test --offline -p atlas-desktop --test overview the_plate_fits -- --nocapture`.
        eprintln!(
            "box {size:?}: plate {:.0}x{:.0} ({:.1} % of the height), caption bottom {:.0}, stories card {:.0}",
            plate.width(),
            plate.height(),
            100.0 * plate.height() / size.y,
            caption.bottom(),
            painted.cards().last().map_or(0.0, |card| card.height()),
        );
    }
    assert!(
        heights.windows(2).all(|pair| pair[0] <= pair[1] + 0.5),
        "the plate must grow with the window: {heights:?}"
    );
    assert!(
        heights[3] > heights[0] * 1.5,
        "the plate is still nearly one height at every size: {heights:?}"
    );
}

/// A caption that does not fit is cut by the painter and says nothing about it — the
/// measured defect was a galley 10 px wider than its scissor at a 700 px window
/// ("33 place(s) in the last 72 h · 0 c…", `/tmp/atlas-audit-layout.md`). The note
/// therefore has to lose its second clause *before* the scissor takes a character.
#[test]
fn the_plate_caption_is_never_cut_by_its_own_scissor() {
    let boxes = [
        Vec2::new(472.0, 450.0),
        Vec2::new(672.0, 550.0),
        Vec2::new(1052.0, 750.0),
        Vec2::new(1452.0, 1130.0),
        Vec2::new(2172.0, 1330.0),
    ];
    let mut hinted = Vec::new();
    for size in boxes {
        let mut page = Page::new(
            size,
            snapshot(stories(4), vec![("SE", 9)], vec![pin("Nairobi", -1.3, 36.8, 2)]),
        );
        let (_, painted) = page.quiet();
        let (note, galley, clip) = painted
            .runs
            .iter()
            .find(|(text, ..)| text.contains("in the last"))
            .map(|(text, galley, clip)| (text, *galley, *clip))
            .unwrap_or_else(|| panic!("no map caption was painted at {size:?}"));
        assert!(
            galley.right() <= clip.right() + 0.5 && galley.left() >= clip.left() - 0.5,
            "at {size:?} the caption {note:?} is {:?} wide but its scissor is {:?}",
            galley,
            clip
        );
        // Whatever the width, the first clause is whole: the shortest form still says how
        // many places and over what window, which is the sentence a reader needs.
        let first = note.split(" · ").next().unwrap_or(note);
        assert!(
            first.starts_with("1 place in the last three days"),
            "at {size:?} the caption's first clause was cut: {first:?}"
        );
        hinted.push(note.contains("coastlines"));
    }
    assert!(
        !hinted[0],
        "the narrowest box kept the hint clause, which is what used to be clipped"
    );
    assert!(
        hinted[4],
        "the widest box dropped the hint clause it has room for"
    );
}

// ------------------------------------------------------- 2. the page is its cards
/// The page ends where the box ends: the stories card is the rest of the page, the gap
/// between the two cards never changes, and nothing is painted below the box — which is
/// what "the page no longer needs the window's scroll area" means, measured.
#[test]
fn the_page_is_the_sum_of_its_cards_and_never_a_typed_height() {
    let mut story_heights = Vec::new();
    let mut map_heights = Vec::new();
    for size in [Vec2::new(472.0, 450.0), Vec2::new(1052.0, 750.0)] {
        let mut page = Page::new(
            size,
            snapshot(stories(20), vec![("SE", 9)], vec![pin("Stockholm", 59.3, 18.1, 2)]),
        );
        let (_, painted) = page.quiet();
        let cards = painted.cards();
        assert_eq!(cards.len(), 2, "two cards, found {:?}", cards);
        let (map, stories) = (cards[0], cards[1]);
        let gap = stories.top() - map.bottom();
        assert!(
            (0.5..=24.0).contains(&gap),
            "the gap between the cards is {gap} at {size:?}"
        );
        assert!(
            (stories.bottom() - size.y).abs() <= 1.5,
            "the stories card ends at {} in a {} px page",
            stories.bottom(),
            size.y
        );
        assert!(
            map.bottom() < stories.top() && map.height() > 100.0,
            "the map card collapsed: {map:?}"
        );
        // nothing painted below the box, and nothing to the right of it
        for (rect, _) in &painted.fills {
            assert!(
                rect.bottom() <= size.y + 1.0 && rect.right() <= size.x + 1.0,
                "a fill {rect:?} leaves the page box {size:?}"
            );
        }
        for (what, rect) in &painted.texts {
            assert!(
                rect.bottom() <= size.y + 1.0 && rect.right() <= size.x + 1.0,
                "the text {what:?} at {rect:?} is outside the {size:?} box"
            );
        }
        // The column is at least a card, at every size — and on a page that has the room
        // at least one story row inside it (`story_row` is 61 px at the type scale, the
        // card's chrome 52). On the smallest audited page (472×450) it is a sliver that
        // scrolls: the map keeps its floor there, because a squeezed world map was the
        // defect this page was reported for, and a column the reader can scroll is not.
        assert!(
            stories.height() >= 52.0,
            "the stories card is {} px tall at {size:?}, less than its own chrome",
            stories.height()
        );
        if size.x >= 1_000.0 {
            assert!(
                stories.height() >= 61.0 + 52.0,
                "the column is {} px tall at {size:?} — not one row on a page this big",
                stories.height()
            );
        }
        story_heights.push(stories.height());
        map_heights.push(map.height());
    }
    // The window's extra height goes to the map first (that is what "the map is the hero"
    // means), and the column keeps its two rows; what must not happen is a page that stops
    // before the box does, which is asserted above at both sizes.
    assert!(
        map_heights[1] > map_heights[0] + 100.0,
        "the map did not take the room a taller window gives it: {map_heights:?}"
    );
}

// ---------------------------------------------------------- 3. the column scrolls

/// A wheel over the stories column moves the rows and leaves the map where it was.
///
/// Before this page had a scroll area of its own, the window wrapped the whole page, so a
/// wheel anywhere moved the map too.
#[test]
fn the_stories_column_scrolls_inside_the_page_and_the_plate_stays_put() {
    let mut page = Page::new(
        Vec2::new(700.0, 520.0),
        snapshot(stories(30), vec![("SE", 9)], vec![pin("Stockholm", 59.3, 18.1, 2)]),
    );
    let (_, before) = page.quiet();
    let plate_before = before.plate();
    let first = "Story 1 about the harbour";
    assert!(before.text().contains(first), "the first row is not drawn");

    // The wheel goes into the column, at the point the first row is drawn at: a scroll
    // area hears a wheel over the pixels it shows, not over its scrolled-out content.
    let column = page.widget(overview::story_id(1));
    let (choice, after) = page.wheel(column.center(), -240.0);
    assert!(choice.nothing(), "a wheel is not a click: {choice:?}");
    assert_eq!(
        after.plate(),
        plate_before,
        "the wheel moved the map; the column is supposed to scroll instead"
    );
    assert!(
        !after.text().contains(first),
        "the first row is still drawn after a wheel past it — the column did not scroll:\n{}",
        after.text()
    );
}

// ------------------------------------------------------------- 4. the two doors

/// A click on a chip answers that **country**, with the number the chip printed, and never a
/// story.
///
/// The chips under the plate are one row of `Archive::countries` each — stories grouped by
/// their `iso` — so the door has to say *country*: that is what makes the window's read the
/// same grouping the chip counted (`app::stories_for`). The old answer was a bare `String`,
/// which the window read as a *place label*: measured on the shipped archive the `USA` chip
/// said 105 stories and the list its click opened held 0, because every United States story
/// names a city. `tests/map_audit.rs` pins that on the real archive; this pins the type the
/// page hands over, and that the number in it is the number on the chip.
#[test]
fn a_click_on_a_place_chip_answers_that_place_and_never_a_story() {
    let mut page = Page::new(
        Vec2::new(900.0, 620.0),
        snapshot(
            stories(2),
            vec![("SE", 12), ("DE", 7)],
            vec![pin("Stockholm", 59.3, 18.1, 2)],
        ),
    );
    let (_, painted) = page.quiet();
    assert!(
        painted.text().contains("SE 12") && painted.text().contains("DE 7"),
        "the rollup is not drawn:\n{}",
        painted.text()
    );

    let (choice, _) = page.click_widget(overview::place_id("SE"));
    assert_eq!(
        choice,
        Choice {
            story: None,
            place: Some(Door::country("SE", 12))
        },
        "a click on the SE chip must answer the country and no story"
    );
    let door = choice.door().expect("the SE chip answered no door");
    assert_eq!(
        door.kind,
        Kind::Country,
        "the SE chip is a row of `Archive::countries` (grouped by `iso`) but its door says \
         {:?}: the window would read the place *label* and open nothing",
        door.kind
    );
    assert_eq!(
        door.count, 12,
        "the door carries a different number than the chip printed"
    );

    let (choice, _) = page.click_widget(overview::place_id("DE"));
    assert_eq!(choice.place, Some(Door::country("DE", 7)));
    assert_eq!(choice.story, None);
}

/// A click on a story row answers its id, and never a place.
#[test]
fn a_click_on_a_story_row_answers_that_story_and_never_a_place() {
    let mut page = Page::new(
        Vec2::new(900.0, 620.0),
        snapshot(
            stories(6),
            vec![("SE", 12)],
            vec![pin("Stockholm", 59.3, 18.1, 2)],
        ),
    );
    let (choice, _) = page.click_widget(overview::story_id(1));
    assert_eq!(
        choice,
        Choice {
            story: Some(1),
            place: None
        },
        "a click on the first row must answer that story and no place"
    );
}

/// The keyboard is a door too: a focused chip answers Enter the way it answers a click,
/// and the ring that says where the keyboard is is painted on the chip itself.
#[test]
fn a_focused_chip_answers_enter_and_wears_the_focus_ring() {
    let mut page = Page::new(
        Vec2::new(900.0, 620.0),
        snapshot(stories(3), vec![("SE", 12)], vec![pin("Nairobi", -1.3, 36.8, 2)]),
    );
    let chip = page.widget(overview::place_id("SE"));
    page.ctx.memory_mut(|memory| memory.request_focus(overview::place_id("SE")));
    let (choice, painted) = page.frame(vec![egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }]);
    assert_eq!(
        choice.place,
        Some(Door::country("SE", 12)),
        "Enter on the focused chip must answer the country it counts"
    );
    let focus = theme::focus_stroke().color;
    assert!(
        painted
            .rings
            .iter()
            .any(|(rect, colour)| rect.shrink(1.0).contains_rect(chip.shrink(2.0)) && *colour == focus),
        "the focused chip painted no focus ring in {focus:?}: {:?}",
        painted.rings
    );
}

// ------------------------------------------------------------- 5. the map still works

/// The dot on the plate is still a door: a click on it answers the newest story filed at
/// that place.
///
/// This is `tests/e2e_map_click.rs`'s path, proved against the page: the click is aimed at
/// the pixel the map painted the dot at, and the answer is the pin's article.
#[test]
fn a_click_on_a_dot_on_the_plate_still_answers_that_place_s_newest_story() {
    let mut page = Page::new(
        Vec2::new(1200.0, 820.0),
        snapshot(Vec::new(), vec![("Nairobi", 3)], vec![pin("Nairobi", -1.3, 36.8, 42)]),
    );
    let (_, painted) = page.quiet();
    let plate = painted.plate();
    let dots: Vec<(Pos2, f32)> = painted
        .dots
        .iter()
        .copied()
        .filter(|(at, _)| plate.contains(*at))
        .collect();
    assert_eq!(dots.len(), 1, "one pin, one dot on the plate: {dots:?}");
    let (at, _) = dots[0];
    let (choice, _) = page.click(at);
    assert_eq!(
        choice,
        Choice {
            story: Some(42),
            place: None
        },
        "the dot at {at:?} did not answer its story"
    );
}

// ------------------------------------------------------- 6. hostile names, empty page

/// A place name is the archive's to write, so it must not decide the page's width: a
/// 500-character name, an empty one, digits and a single emoji all stay inside the card,
/// inside the chip bound, and one row tall.
#[test]
fn hostile_place_names_do_not_break_the_widths() {
    let long = "x".repeat(500);
    let names = ["SE", "1234", "", "🇸🇪", "a\nb", long.as_str()];
    let mut page = Page::new(
        Vec2::new(700.0, 520.0),
        snapshot(
            stories(3),
            names.iter().map(|name| (*name, 5)).collect(),
            vec![pin("Stockholm", 59.3, 18.1, 2)],
        ),
    );
    let (_, painted) = page.quiet();
    let size = page.size;
    for (rect, _) in &painted.fills {
        assert!(
            rect.right() <= size.x + 1.0 && rect.bottom() <= size.y + 1.0,
            "a chip widened the page: {rect:?} in {size:?}"
        );
    }
    let cards = painted.cards();
    let map_card = cards[0];
    for name in names {
        let rect = page.widget(overview::place_id(name));
        assert!(
            rect.width() <= 180.0 + 0.5,
            "the chip for {name:?} is {} px wide",
            rect.width()
        );
        assert!(
            map_card.left() - 0.5 <= rect.left() && rect.right() <= map_card.right() + 0.5,
            "the chip for {name:?} ({rect:?}) left its card ({map_card:?})"
        );
    }
    // and the count is still drawn for a name that is not a name
    assert!(
        painted.text().contains("unknown place 5"),
        "an empty place lost its words:\n{}",
        painted.text()
    );
    assert!(
        painted.text().contains("SE 5"),
        "the ordinary place lost its words:\n{}",
        painted.text()
    );
}

/// The three empty states still say what would fill them.
///
/// The rollup's sentence used to say "No place has a story in this window yet", and the
/// chips are not places — they are one row of `Archive::countries`, stories grouped by their
/// `iso`. The old wording was the same mistake the door made (a country word read as a place
/// label, which opened nothing), so the expectation follows the page's corrected copy: the
/// empty state names the *country* rollup it will fill with.
#[test]
fn the_empty_page_says_what_would_fill_each_part() {
    let mut page = Page::new(
        Vec2::new(900.0, 620.0),
        snapshot(Vec::new(), Vec::new(), Vec::new()),
    );
    let (choice, painted) = page.quiet();
    assert!(choice.nothing(), "{choice:?}");
    let text = painted.text();
    for (what, sentence) in [
        ("the map", "carries a coordinate"),
        ("the country rollup", "No country has a story in this window yet"),
        ("the stories", "No stories in this window yet"),
    ] {
        assert!(
            text.contains(sentence),
            "{what} has no empty state; painted:\n{text}"
        );
    }
}
