//! The plate's pin card, pinned: what a reader gets from a pin with a pointer and with the
//! keyboard, read back out of the frames the view paints.
//!
//! `views/map.rs` draws the plate, and `views::map::MapView::show` answers the pin a press
//! landed on. The card is the box that appears over the plate for the pin a reader is on:
//! it is *painted*, not a widget, so every claim below is made against the shapes of a real
//! frame rather than against fields. One page, six claims:
//!
//! 1. **Hover paints the card; leaving clears it.** The pointer on a dot paints exactly one
//!    box with text in it, that names the pin's place; the pointer over open water paints none.
//! 2. **A press opens that pin's story, and the card names the same story.** The press answers
//!    the pin's index, whose `article` is the id the shell opens. The card must be *about that
//!    story*: a card that cannot tell two stories at one place apart is a card that offers a
//!    press the reader was never shown.
//! 3. **Escape clears the pin and gives the plate its focus back**, so the keyboard is not
//!    stranded on a plate that is still showing a card.
//! 4. **Tab and the arrows walk the pins, and Enter opens the one that is picked** — in an order
//!    that does not change between runs, so "the next pin" means something.
//! 5. **The card never paints outside the plate**, at three page sizes: every box, every line of
//!    text and every clipping rectangle a frame paints is inside the plate rect.
//! 6. **A pin whose story is gone says so.** A dot whose `article` is 0 is a door that opens
//!    nothing; its card must carry a sentence the live card does not, not fall silent.
//!
//! ## What passes, and what fails today
//!
//! Four tests are green: hover paints a card that names its place and open water clears it;
//! Escape clears the pin and the plate takes its focus ring back; Tab focuses the plate and shows
//! it; and no card — hovered or picked — paints a pixel outside the plate at 900×600, 1100×820 or
//! 1280×800.
//!
//! Three are red, and each one names what is missing rather than what is wrong in the test:
//!
//! * `pressing_a_pin_opens_that_pins_story_and_the_card_names_it` — the id half passes for all
//!   five pins, but the card is `cluster_label(places, stories, place)` (`src/views/map.rs:962`,
//!   painted at 1260–1280), which reads "Kinshasa · 9 stories". Two pins at one place that open
//!   story 11 and story 99 paint the same card, because a `Pin` carries the story's **id** and no
//!   headline.
//! * `a_pin_whose_story_is_gone_paints_a_sentence` — a pin with `article = 0` paints exactly the
//!   card of a pin with story 11: the dot is a door that opens nothing, and nothing says so.
//! * `the_arrows_walk_the_pins_and_enter_opens_the_picked_one` — the arrows pan the camera
//!   (`src/views/map.rs:1133-1140`) and Enter is read nowhere in the file, so a keyboard reader
//!   cannot pick a pin at all; `selected` is only ever set by a click (`:1250`) and cleared by
//!   Escape (`:1170`).
//!
//! They are written as the requirement rather than left out, because a card that cannot say which
//! story it opens is the defect a reader reports as "the map is broken".
//!
//! Nothing here opens a window or a socket, and no archive is read: the pins are the same rows
//! `data::Archive::pins` returns, handed in by hand.

use atlas_desktop::data::Pin;
use atlas_desktop::theme;
use atlas_desktop::views::map::MapView;
use egui::{Color32, Pos2, Rect, Vec2};

// --------------------------------------------------------------------------- reading a frame

/// What one frame of the plate painted.
struct Painted {
    /// The plate: the box the map is drawn in, and the only thing a card may share a pixel with.
    plate: Rect,
    /// The dots, as (centre, radius): filled circles in ink.
    dots: Vec<(Pos2, f32)>,
    /// The card: filled boxes on the plate that are not the plate itself.
    cards: Vec<Rect>,
    /// Every line of text, with the box it was drawn in.
    texts: Vec<(Rect, String)>,
    /// What each shape was clipped to.
    clips: Vec<Rect>,
    /// The plate's focus ring: a rectangle stroked at ring weight, with no fill.
    rings: usize,
    /// The pin `show` answered this frame, if any.
    answered: Option<usize>,
}

impl Default for Painted {
    fn default() -> Painted {
        Painted {
            plate: Rect::NOTHING,
            dots: Vec::new(),
            cards: Vec::new(),
            texts: Vec::new(),
            clips: Vec::new(),
            rings: 0,
            answered: None,
        }
    }
}

impl Painted {
    /// The lines the card painted: the text whose box lies inside a card box.
    fn card_lines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (at, words) in &self.texts {
            if self
                .cards
                .iter()
                .any(|card| card.intersects(*at) && !words.trim().is_empty())
            {
                out.push(words.clone());
            }
        }
        out
    }
}

fn painted(output: &egui::FullOutput) -> Painted {
    let mut out = Painted::default();
    // The filled boxes are collected first and judged afterwards: the card and the plate are the
    // same colour (`theme::PAGE`), so "which one is the plate" is a question about size and not
    // about which arm of a match ran first.
    let mut boxes: Vec<(Rect, Color32)> = Vec::new();
    for clipped in &output.shapes {
        out.clips.push(clipped.clip_rect);
        walk(&clipped.shape, &mut |shape| match shape {
            egui::Shape::Rect(rect) if rect.fill.a() > 0 && rect.rect.is_positive() => {
                boxes.push((rect.rect, rect.fill));
            }
            egui::Shape::Rect(rect)
                if rect.fill == Color32::TRANSPARENT
                    && rect.stroke.color == theme::focus_stroke().color
                    && rect.stroke.width >= theme::focus_stroke().width =>
            {
                out.rings += 1;
            }
            egui::Shape::Circle(circle) if circle.fill == theme::INK => {
                out.dots.push((circle.center, circle.radius));
            }
            egui::Shape::Text(text) => out.texts.push((
                Rect::from_min_size(text.pos, text.galley.size()),
                text.galley.text().to_string(),
            )),
            _ => {}
        });
    }
    let area = |rect: &Rect| rect.width() * rect.height();
    out.plate = boxes
        .iter()
        .filter(|(_, fill)| *fill == theme::PAGE)
        .max_by(|left, right| area(&left.0).total_cmp(&area(&right.0)))
        .map(|(rect, _)| *rect)
        .unwrap_or(Rect::NOTHING);
    // Everything else that was filled is chrome on the plate — the pin's card — so a test can
    // hold the card to the plate's edges without knowing how the card is spelled.
    out.cards = boxes
        .iter()
        .filter(|(rect, _)| *rect != out.plate)
        .map(|(rect, _)| *rect)
        .collect();
    out
}

/// Visit a shape and everything it nests.
fn walk(shape: &egui::Shape, visit: &mut impl FnMut(&egui::Shape)) {
    visit(shape);
    if let egui::Shape::Vec(children) = shape {
        for child in children {
            walk(child, visit);
        }
    }
}

// --------------------------------------------------------------------------- the harness

/// A headless page holding nothing but the plate, at one size.
struct Page {
    ctx: egui::Context,
    size: Vec2,
    /// The clock: `stable_dt` drives the plate's ease, so a frame has to have a time to step by.
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

    /// One frame of the plate with these `pins`, with these events.
    fn draw(&mut self, events: Vec<egui::Event>, view: &mut MapView, pins: &[Pin]) -> Painted {
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
                    answered = view.show(ui, pins, height);
                });
        });
        let mut out = painted(&output);
        out.answered = answered;
        output.drop_without_applying_deltas();
        out
    }

    /// Draw frames until the camera stops moving, and answer the frame that settled.
    ///
    /// The plate eases every change of camera (`Motion::step`), so a position computed from a
    /// half-eased camera is a position the dots are not painted at.
    fn settle(&mut self, view: &mut MapView, pins: &[Pin]) -> Painted {
        let mut last = (view.camera().zoom(), view.camera().offset());
        let mut still = 0;
        let mut out = self.draw(Vec::new(), view, pins);
        for _ in 0..120 {
            out = self.draw(Vec::new(), view, pins);
            let now = (view.camera().zoom(), view.camera().offset());
            if (now.0 - last.0).abs() < 1e-5 && (now.1.x - last.1.x).abs() < 0.01 && (now.1.y - last.1.y).abs() < 0.01 {
                still += 1;
                if still >= 2 {
                    return out;
                }
            } else {
                still = 0;
            }
            last = now;
        }
        out
    }

    /// Move the pointer to `at` and draw, and draw the frame after it.
    ///
    /// Two frames, because `Response::contains_pointer` is a fact about the frame before the
    /// pointer arrived and the frame it arrives in: the hover a reader sees is the second one.
    /// The frame that is answered is the one the card is painted in.
    fn hover(&mut self, at: Pos2, view: &mut MapView, pins: &[Pin]) -> Painted {
        self.draw(vec![egui::Event::PointerMoved(at)], view, pins);
        self.draw(vec![egui::Event::PointerMoved(at)], view, pins)
    }

    /// Press and release at `at`: the gesture a reader makes on a dot.
    fn click(&mut self, at: Pos2, view: &mut MapView, pins: &[Pin]) -> Painted {
        self.draw(vec![egui::Event::PointerMoved(at)], view, pins);
        let mut events = vec![egui::Event::PointerMoved(at)];
        events.extend(button(at, true));
        self.draw(events, view, pins);
        let mut events = vec![egui::Event::PointerMoved(at)];
        events.extend(button(at, false));
        self.draw(events, view, pins)
    }

    /// One key, pressed.
    fn key(&mut self, key: egui::Key, view: &mut MapView, pins: &[Pin]) -> Painted {
        self.draw(
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            view,
            pins,
        )
    }
}

fn button(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }]
}

// --------------------------------------------------------------------------- the fixture

/// Five places, far apart on the plate so each is one dot and one dot only — and all five inside
/// the plate at the narrowest page this file uses (900×600 crops the world to ±135° of
/// longitude and ±90° of latitude, so no place here is past ±100° or ±60°).
///
/// `article` is the id the shell opens when the pin is pressed; it is chosen here so every
/// assertion can name the id it resolved.
fn pins() -> Vec<Pin> {
    vec![
        Pin { place: "Kinshasa".into(), lat: -4.3, lon: 15.3, count: 9, article: 11 },
        Pin { place: "Lima".into(), lat: -12.0, lon: -77.0, count: 4, article: 12 },
        Pin { place: "Oslo".into(), lat: 59.9, lon: 10.7, count: 6, article: 13 },
        Pin { place: "Mexico City".into(), lat: 19.4, lon: -99.1, count: 3, article: 14 },
        Pin { place: "Bengaluru".into(), lat: 13.0, lon: 77.6, count: 7, article: 15 },
    ]
}

/// A fresh plate: the vendored outlines and the opening camera.
fn map_view() -> MapView {
    MapView::new(&atlas_desktop::assets::geojson())
}

/// Where a pin's dot is painted: the plate's middle, plus the camera's projection.
///
/// This is the same arithmetic the view paints through (`on_plate`), read from the camera the
/// view publishes — so a test can aim at a dot without re-implementing the map.
fn dot_of(plate: Rect, view: &MapView, pin: &Pin) -> Pos2 {
    let point = view.camera().project(atlas_desktop::geo::Geo::new(pin.lon as f32, pin.lat as f32));
    Pos2::new(plate.center().x + point.x, plate.center().y + point.y)
}

/// A point of the plate at least `clearance` px from every dot: open water.
fn open_water(drawn: &Painted, clearance: f32) -> Pos2 {
    let plate = drawn.plate;
    let mut y = plate.top() + clearance;
    while y < plate.bottom() - clearance {
        let mut x = plate.left() + clearance;
        while x < plate.right() - clearance {
            let at = Pos2::new(x, y);
            if drawn
                .dots
                .iter()
                .all(|(centre, radius)| centre.distance(at) > radius + clearance)
            {
                return at;
            }
            x += 8.0;
        }
        y += 8.0;
    }
    panic!("the plate has no open water {clearance} px from every dot");
}

/// The painted dot a pin is, checked: a fixture whose dots are not where the camera says they
/// are would make every click below a click on open water.
fn dot_at(drawn: &Painted, at: Pos2, pin: &Pin) -> Pos2 {
    drawn
        .dots
        .iter()
        .map(|(centre, _)| *centre)
        .find(|centre| centre.distance(at) <= 1.0)
        .unwrap_or_else(|| {
            panic!(
                "no dot is painted at {at:?}, where the camera puts {} ({}, {}); the plate painted \
                 {} dots",
                pin.place,
                pin.lat,
                pin.lon,
                drawn.dots.len()
            )
        })
}

// --------------------------------------------------------------------------- 1. hover

/// Hovering a pin paints its card, and leaving the pin clears it.
///
/// The card is a box with text in it over the plate: it names the place whose dot the pointer is
/// on, it sits beside that dot, and it goes away when the pointer leaves the dot. A card that
/// stayed after the pointer left would be a box a reader cannot dismiss.
#[test]
fn hovering_a_pin_paints_its_card_and_leaving_clears_it() {
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let pins = pins();
    let settled = page.settle(&mut view, &pins);
    let plate = settled.plate;
    assert!(
        plate.width() > 900.0 && plate.height() > 500.0,
        "the plate is {:?} in a 1000x620 page",
        plate.size()
    );

    let mut checked = 0;
    for pin in &pins {
        let at = dot_at(&settled, dot_of(plate, &view, pin), pin);
        let hovered = page.hover(at, &mut view, &pins);
        assert_eq!(
            hovered.cards.len(),
            1,
            "hovering {} (article {}) painted {} card boxes, not one",
            pin.place,
            pin.article,
            hovered.cards.len()
        );
        let card = hovered.cards[0];
        assert!(
            card.expand(160.0).contains(at),
            "the card for {} (article {}) is painted at {card:?}, nowhere near its dot at {at:?}",
            pin.place,
            pin.article
        );
        let lines = hovered.card_lines();
        assert!(
            !lines.is_empty(),
            "the card for {} (article {}) is an empty box: it painted no text at all",
            pin.place,
            pin.article
        );
        assert!(
            lines.iter().any(|line| line.contains(&pin.place)),
            "the card for {} (article {}) does not name the place it belongs to: {lines:?}",
            pin.place,
            pin.article
        );
        assert!(
            hovered.answered.is_none(),
            "hovering {} answered story {:?}: a pointer moving over a dot is not a press",
            pin.place,
            hovered.answered
        );
        checked += 1;
    }

    // Off the dots: no card at all.
    let water = open_water(&settled, 60.0);
    let away = page.hover(water, &mut view, &pins);
    assert!(
        away.cards.is_empty(),
        "the pointer left the dots for open water at {water:?} and {} card(s) are still painted \
         ({:?})",
        away.cards.len(),
        away.card_lines()
    );
    println!(
        "hover: {checked} pins, each painting one card that names its place; open water at \
         {water:?} paints none"
    );
}

// --------------------------------------------------------------------------- 2. press

/// A press on a pin opens that pin's story, and the card is about that story.
///
/// The id half is a fact about the press: `show` answers the pin's index, and the pin carries the
/// story the shell opens — reported below for every pin. The card half is the requirement: a card
/// that reads the same for two different stories at one place offers a press the reader was never
/// shown, so a card that cannot tell story 11 from story 99 is not a card for a story.
#[test]
fn pressing_a_pin_opens_that_pins_story_and_the_card_names_it() {
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let pins = pins();
    let settled = page.settle(&mut view, &pins);
    let plate = settled.plate;

    // ---- the id: every pin opens its own story
    for (index, pin) in pins.iter().enumerate() {
        let at = dot_at(&settled, dot_of(plate, &view, pin), pin);
        let pressed = page.click(at, &mut view, &pins);
        assert_eq!(
            pressed.answered,
            Some(index),
            "pressing {} (article {}) answered {:?}, not pin {index}",
            pin.place,
            pin.article,
            pressed.answered
        );
        assert_eq!(
            view.selected(),
            Some(index),
            "pressing {} left {:?} picked, not pin {index}",
            pin.place,
            view.selected()
        );
        assert!(
            pin.article > 0,
            "the fixture's pin {} carries story {}, which the shell would not open",
            pin.place,
            pin.article
        );
        let lines = pressed.card_lines();
        assert!(
            lines.iter().any(|line| line.contains(&pin.place)),
            "the card painted for pin {index} ({}) does not name it: {lines:?}",
            pin.place
        );
        println!(
            "press: pin {index} ({}), story id {} — the card says {:?}",
            pin.place, pin.article, lines
        );
    }

    // ---- the card must be about the story it opens
    //
    // The same place, the same count, one field different: the story the pin opens. If the card
    // comes out the same, the card is about the *place* and not about the story — and a reader
    // cannot tell which story a press will open, which is the whole job of the card.
    let one = vec![Pin {
        place: "Kinshasa".into(),
        lat: -4.3,
        lon: 15.3,
        count: 9,
        article: 11,
    }];
    let other = vec![Pin {
        place: "Kinshasa".into(),
        lat: -4.3,
        lon: 15.3,
        count: 9,
        article: 99,
    }];
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let first = page.settle(&mut view, &one);
    let at = dot_at(&first, dot_of(first.plate, &view, &one[0]), &one[0]);
    let story_11 = page.hover(at, &mut view, &one).card_lines();
    let story_99 = page.hover(at, &mut view, &other).card_lines();
    assert_ne!(
        story_11, story_99,
        "the card for Kinshasa is identical whether the pin opens story 11 or story 99: {story_11:?}. \
         The card cannot name the story its press opens — the plate is handed `&[Pin]`, and a `Pin` \
         carries the story's id ({}, {}) and no headline, so a press is a press in the dark.",
        one[0].article, other[0].article
    );
}

// --------------------------------------------------------------------------- 3. escape

/// Escape clears the pin and gives the plate its focus back.
///
/// A reader who has picked a dot and pressed the key that means "not that" must be left on the
/// plate, not stranded: the card goes, nothing opens, and the keyboard still belongs to the plate.
#[test]
fn escape_clears_the_pin_and_returns_focus_to_the_plate() {
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let pins = pins();
    let settled = page.settle(&mut view, &pins);
    let at = dot_at(&settled, dot_of(settled.plate, &view, &pins[0]), &pins[0]);
    let pressed = page.click(at, &mut view, &pins);
    assert_eq!(
        pressed.answered,
        Some(0),
        "the setup did not open a story: pressing {} answered {:?}",
        pins[0].place,
        pressed.answered
    );
    assert_eq!(view.selected(), Some(0), "the setup picked no pin");

    // The pointer leaves the dot: the *pin* is still picked and the card stays up for it — a
    // reader who has picked a dot can move the pointer away and keep reading. What follows is
    // therefore about Escape and not about the pointer.
    let water = open_water(&settled, 60.0);
    let away = page.hover(water, &mut view, &pins);
    assert_eq!(
        view.selected(),
        Some(0),
        "moving the pointer off the dot dropped the pin, so there is nothing for Escape to clear \
         and nothing for the card to stay up for"
    );
    assert_eq!(
        away.cards.len(),
        1,
        "the picked pin's card went away when the pointer left its dot: a reader who has picked a \
         dot cannot move the pointer away and still read it"
    );
    assert!(
        away.card_lines()
            .iter()
            .any(|line| line.contains(&pins[0].place)),
        "the card left up for the picked pin does not name it: {:?}",
        away.card_lines()
    );

    let escaped = page.key(egui::Key::Escape, &mut view, &pins);
    assert_eq!(
        view.selected(),
        None,
        "Escape left pin {:?} picked ({}): the reader cannot get out of the card",
        view.selected(),
        pins[0].place
    );
    assert!(
        escaped.answered.is_none(),
        "Escape opened story {:?}: the key that cancels a pin must not open one",
        escaped.answered
    );
    let after = page.draw(Vec::new(), &mut view, &pins);
    assert!(
        after.rings >= 1,
        "the plate does not show that it has the keyboard back after Escape: no focus ring was \
         painted, so the arrows that pan or step the plate have nothing to act on"
    );
    assert!(
        after.cards.is_empty(),
        "the card is still painted after Escape cleared the pin: {:?}",
        after.card_lines()
    );
    println!(
        "escape: pin 0 ({}, story {}) was picked, Escape cleared it and the plate paints its \
         focus ring",
        pins[0].place, pins[0].article
    );
}

// --------------------------------------------------------------------------- 4. keyboard

/// Tab focuses the plate, and the plate shows it with the ring a keyboard reader needs.
///
/// This is the first half of the keyboard path, and it is the half that works: without a ring a
/// reader cannot tell whether the keys are going to the plate or to the page.
#[test]
fn tab_focuses_the_plate_and_the_plate_shows_it() {
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let pins = pins();
    let settled = page.settle(&mut view, &pins);

    let unfocused = page.hover(open_water(&settled, 60.0), &mut view, &pins);
    assert_eq!(
        unfocused.rings, 0,
        "the plate painted a focus ring before anything focused it"
    );
    let focused = page.key(egui::Key::Tab, &mut view, &pins);
    assert!(
        focused.rings >= 1,
        "Tab did not focus the plate, or the plate did not show it: no focus ring was painted"
    );
    assert_eq!(
        view.selected(),
        None,
        "Tab picked pin {:?} before any arrow was pressed",
        view.selected()
    );
    println!("tab: the plate paints its focus ring, and no pin is picked yet");
}

/// The arrows walk the pins in a stable order, and Enter opens the pin that is picked.
///
/// The order has to be stable to be an order: the same key presses on the same pins must pick the
/// same dots every time, and walking right and then left must come back. Enter opens what the
/// arrows picked — the same id a press on that dot answers — so the keyboard reaches every story
/// the pointer can.
#[test]
fn the_arrows_walk_the_pins_and_enter_opens_the_picked_one() {
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let pins = pins();
    let settled = page.settle(&mut view, &pins);
    let plate = settled.plate;
    page.key(egui::Key::Tab, &mut view, &pins);

    // Walk right, then left, and read the order back.
    let mut walked: Vec<Option<usize>> = Vec::new();
    let before = (view.camera().zoom(), view.camera().offset().x, view.camera().offset().y);
    page.key(egui::Key::ArrowRight, &mut view, &pins);
    let mut picked = view.selected();
    walked.push(picked);
    assert!(
        picked.is_some(),
        "ArrowRight on a focused plate picked no pin: the plate paints {} dots and no key reaches \
         any of them. The arrow went to the camera instead — zoom ×{:.2} at ({:.1}, {:.1}) before, \
         ×{:.2} at ({:.1}, {:.1}) after — so a reader without a pointer cannot open a single story \
         on this plate.",
        settled.dots.len(),
        before.0,
        before.1,
        before.2,
        view.camera().zoom(),
        view.camera().offset().x,
        view.camera().offset().y
    );
    for _ in 0..3 {
        page.key(egui::Key::ArrowRight, &mut view, &pins);
        let next = view.selected();
        assert!(
            next.is_some(),
            "the fourth ArrowRight left no pin picked (was {:?})",
            picked
        );
        assert_ne!(
            next, picked,
            "ArrowRight stayed on pin {:?} ({}) instead of moving to the next one",
            next,
            pins.get(next.unwrap_or(0)).map(|pin| pin.place.as_str()).unwrap_or("?")
        );
        walked.push(next);
        picked = next;
    }
    // …and back again.
    for _ in 0..3 {
        page.key(egui::Key::ArrowLeft, &mut view, &pins);
    }
    assert_eq!(
        view.selected(),
        walked[0],
        "walking three pins right and three back landed on {:?}, not on the pin it started from \
         ({:?}); the order is not stable. Walked: {walked:?}",
        view.selected(),
        walked[0]
    );

    // The order is the same from a second, fresh plate: nothing about it depends on the frame.
    let mut again_page = Page::new(1000.0, 620.0);
    let mut again = map_view();
    let again_settled = again_page.settle(&mut again, &pins);
    assert_eq!(
        again_settled.plate, plate,
        "the second plate is not the same box as the first"
    );
    again_page.key(egui::Key::Tab, &mut again, &pins);
    let mut second: Vec<Option<usize>> = Vec::new();
    for _ in 0..4 {
        again_page.key(egui::Key::ArrowRight, &mut again, &pins);
        second.push(again.selected());
    }
    assert_eq!(
        second, walked,
        "the arrows walked {walked:?} on one plate and {second:?} on another: the order is not \
         stable between runs"
    );

    // Enter opens the story the arrows picked: the id a press on that dot answers.
    let picked = view.selected().expect("the walk left a pin picked");
    let opened = page.key(egui::Key::Enter, &mut view, &pins);
    assert_eq!(
        opened.answered,
        Some(picked),
        "Enter opened {:?}, not the picked pin {picked} ({})",
        opened.answered,
        pins.get(picked).map(|pin| pin.place.as_str()).unwrap_or("?")
    );
    let stories: Vec<i64> = walked
        .iter()
        .map(|slot| {
            slot.and_then(|index| pins.get(index))
                .map(|pin| pin.article)
                .unwrap_or(-1)
        })
        .collect();
    println!(
        "keyboard: the arrows walked {walked:?} (stories {stories:?}), Enter opened pin {picked} \
         (story {})",
        pins[picked].article
    );
}

// --------------------------------------------------------------------------- 5. the plate edge

/// The card never paints outside the plate, at the three page sizes the window has to work at.
///
/// A card that hangs over the plate's edge is a card drawn on the page under the map — over the
/// rollup, over a story row, over whatever is beside it. The pointer is put on every pin at every
/// size, and the corner-most dots are tried first, because those are the ones a box has to turn
/// around.
#[test]
fn the_card_never_paints_outside_the_plate() {
    for (width, height) in [(900.0, 600.0), (1100.0, 820.0), (1280.0, 800.0)] {
        let mut page = Page::new(width, height);
        let mut view = map_view();
        let pins = pins();
        let settled = page.settle(&mut view, &pins);
        let plate = settled.plate;
        assert!(
            (plate.width() - width).abs() < 1.0 && (plate.height() - height).abs() < 1.0,
            "the plate is {:?} in a {width}x{height} page",
            plate.size()
        );
        let mut visited = 0;
        let mut tightest = f32::INFINITY;
        for (index, pin) in pins.iter().enumerate() {
            let at = dot_of(plate, &view, pin);
            if !plate.contains(at) {
                // A dot outside the plate is not painted and cannot be hovered; the sizes here
                // are chosen so that all five are on the plate, which is checked below.
                continue;
            }
            let hovered = page.hover(at, &mut view, &pins);
            assert!(
                !hovered.cards.is_empty(),
                "at {width}x{height} hovering pin {index} ({}) painted no card at all",
                pin.place
            );
            for card in &hovered.cards {
                let margin = (card.left() - plate.left())
                    .min(plate.right() - card.right())
                    .min(card.top() - plate.top())
                    .min(plate.bottom() - card.bottom());
                if margin < tightest {
                    tightest = margin;
                }
                assert!(
                    plate.contains_rect(*card),
                    "at {width}x{height} the card for pin {index} ({}) is painted at {card:?}, \
                     outside the plate {plate:?}",
                    pin.place
                );
            }
            for (box_, words) in &hovered.texts {
                assert!(
                    plate.expand(0.5).contains_rect(*box_),
                    "at {width}x{height} the line {words:?} runs to {box_:?}, outside the plate \
                     {plate:?}"
                );
            }
            for clip in &hovered.clips {
                assert!(
                    plate.expand(0.5).contains_rect(*clip),
                    "at {width}x{height} a shape was clipped to {clip:?}, outside the plate \
                     {plate:?}"
                );
            }
            // The card of a *picked* pin stays up when the pointer leaves, so it is the one that
            // has to be inside the plate too — and at the plate's edge it is the one a reader is
            // most likely to have in front of them.
            let picked = page.click(at, &mut view, &pins);
            assert_eq!(
                picked.answered,
                Some(index),
                "at {width}x{height} pressing pin {index} ({}) answered {:?}",
                pin.place,
                picked.answered
            );
            let away = page.hover(open_water(&hovered, 40.0), &mut view, &pins);
            assert!(
                !away.cards.is_empty(),
                "at {width}x{height} the picked pin {index} ({}) lost its card when the pointer \
                 moved away",
                pin.place
            );
            for card in &away.cards {
                assert!(
                    plate.contains_rect(*card),
                    "at {width}x{height} the card of the *picked* pin {index} ({}) is painted at \
                     {card:?}, outside the plate {plate:?}",
                    pin.place
                );
            }
            visited += 1;
        }
        assert!(
            visited >= 4,
            "at {width}x{height} only {visited} of {} pins are on the plate, so the edge was \
             barely tested",
            pins.len()
        );
        println!(
            "{width}x{height}: {visited} pins hovered, every card inside the plate (plate \
             {:.0}x{:.0}); the closest a card came to leaving it was {tightest:.1} px",
            plate.width(),
            plate.height()
        );
    }
}

// --------------------------------------------------------------------------- 6. no story

/// A pin whose story is gone says so, instead of offering a press that opens nothing.
///
/// `article = 0` is the archive's way of saying "this place has no story now": the shell opens
/// nothing for it (`pin.article > 0`). The card for such a pin must carry a sentence the live
/// card does not — the same place, the same count, one field different — because a reader who is
/// shown the same card twice cannot tell the door that opens from the door that does not.
#[test]
fn a_pin_whose_story_is_gone_paints_a_sentence() {
    let live = vec![Pin {
        place: "Kinshasa".into(),
        lat: -4.3,
        lon: 15.3,
        count: 9,
        article: 11,
    }];
    let gone = vec![Pin {
        place: "Kinshasa".into(),
        lat: -4.3,
        lon: 15.3,
        count: 9,
        article: 0,
    }];
    let mut page = Page::new(1000.0, 620.0);
    let mut view = map_view();
    let settled = page.settle(&mut view, &live);
    let at = dot_at(&settled, dot_of(settled.plate, &view, &live[0]), &live[0]);

    let with_story = page.hover(at, &mut view, &live);
    let with_story_lines = with_story.card_lines();
    assert!(
        !with_story_lines.is_empty(),
        "the card for {} (story {}) is empty, so there is nothing to compare",
        live[0].place,
        live[0].article
    );

    let without = page.hover(at, &mut view, &gone);
    let without_lines = without.card_lines();
    assert!(
        !without_lines.is_empty(),
        "the card for {} with no story (article {}) is an empty box: a reader sees a dot, a \
         card with nothing in it, and a press that opens nothing",
        gone[0].place,
        gone[0].article
    );
    assert_ne!(
        without_lines, with_story_lines,
        "the card for {} with no story (article {}) reads exactly like the card with story {}: \
         {without_lines:?}. The dot is a door that opens nothing, and nothing on the plate says so.",
        gone[0].place, gone[0].article, live[0].article
    );
    assert!(
        without_lines
            .iter()
            .any(|line| !with_story_lines.contains(line)),
        "the card for {} with no story (article {}) adds no sentence of its own: {without_lines:?} \
         against {with_story_lines:?}. Turning the story off must be *said*, not merely omitted — \
         the reader is looking at a card that still looks like a door.",
        gone[0].place,
        gone[0].article
    );
    println!(
        "no story: live card {with_story_lines:?} / card with article 0 {without_lines:?}"
    );
}
