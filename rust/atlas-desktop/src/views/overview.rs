//! Overview: the window in one page.
//!
//! Three parts, in the order a reader opens the window for: a heading that says
//! what the archive holds in words, the world map as the hero at the full width of
//! the page, and then the stories of the window as rows — the time, the source and
//! one line of the lede — each one a door into the reading pane. The window's
//! briefing sits **between** the map card and the column: the map answers "where", the briefing
//! answers "what is there", and the stories are themselves the column. Its card is not the
//! page's to size — `views::brief` draws whatever the agent wrote — so the page **measures** it
//! ([`measure_brief`]: one throwaway draw in a clipped, id-salted child `Ui`) and gives it
//! exactly that height. Under the plate, the
//! country rollup: one chip per country, and those are doors too, because "what happened in
//! the USA?" is a question this page can answer by handing the *country* back to its
//! caller — a `Choice` that says which grouping counted the chip ([`Door`]), so the list the
//! window opens is the same set of rows the chip counted. A chip whose word and whose read
//! disagree is a dead door: the shipped archive's `USA` chip counted 105 stories and opened
//! none, which is what [`Kind`] exists to prevent.
//!
//! **No block is clipped by the plan.** Two defects, both read off frames at 1280×800 and
//! 900×620 (`tests/brief_on_screen.rs`), put that sentence here:
//!
//! * the briefing's band was a typed 160 px, so a card with four bullets painted its fourth
//!   line at y 578 in a band that ended at 582 — a severed line — and its footer was never
//!   drawn at all;
//! * the plate was a *share* of the room left over: 209 px tall in a 1 028 px-wide card that
//!   asks for 489, so the map's own fit cropped the poles; at 900×620, 203 px of 368.
//!
//! So the plate takes its height from its width at the world's aspect ([`plate_height`]), the
//! briefing's card takes the height it measured, the column keeps its floor — and when the
//! three do not fit the box under the heading, **the page scrolls** ([`show_briefed`]) rather
//! than cutting a line or a pole. A card taller than a whole page keeps a band of one page and
//! scrolls inside it, so every line stays reachable. The page owns that one scroll surface: a
//! wheel over the plate zooms the map, a wheel over the column moves the column, and a wheel
//! anywhere else on the page moves the page.
//!
//! A row is a real widget, not a picture of one: `Sense::click` makes it Tab-focusable,
//! egui turns Enter and Space on the focused row into a click, and the two states a
//! reader needs are drawn in the palette's own greys — a plate under the pointer or the
//! keyboard, and a hairline ring for the focus.
//!
//! Nothing here copies the archive. The page reads the caller's lists in place, a row
//! scrolled out of the clip rect is dropped before a glyph is shaped, and the only
//! strings built per frame are the layout jobs egui has to be handed (a job owns its
//! text; there is no borrowing form of it) — and each of those holds the prefix of its
//! text that can still be drawn, not the whole stored string (see [`budget`]).

use std::sync::Arc;

use egui::{Color32, FontId, Galley, Id, Pos2, Sense, Stroke, Vec2};

use super::map::{MapView, Pick};
use super::{card, heading, nf, when};
use crate::data::{Article, Counts, Pin};
use crate::theme;

// ------------------------------------------------------------------- the plate's box

/// The shape a world map wants: about two units of width for one of height.
///
/// Not a style choice — the plate is a plate, and a 2:1 world stretched into a 1:1 box
/// is a different picture of the world. Every other number here is derived from it.
pub const PLATE_ASPECT: f32 = 2.1;

/// The narrowest plate that still reads as a map rather than a strip: 360 px wide, which
/// at [`PLATE_ASPECT`] is ~171 px tall.
pub const MIN_PLATE: f32 = 360.0 / PLATE_ASPECT;

/// The tallest plate the page draws when its box **is** the page: 1 600 px of width at the
/// aspect, ~762 px tall ([`fitted`]).
///
/// The page that draws a briefing does not stop here: it has somewhere to scroll to, and a
/// plate capped at this height inside a 2 136 px-wide card is 2.8:1 of world — the poles cut
/// off, which is the defect this file was reported for. A page with no card under the plate
/// has no second scroll surface, so it keeps the ceiling.
pub const MAX_PLATE: f32 = 1_600.0 / PLATE_ASPECT;

/// One story row at the type scale, from [`story_row`]: 8 + title + 3 + small + 3 + small
/// + 8 = 61 px (title 15 px, small 12 px, both at the sizes that row asks for).
const ROW_HEIGHT: f32 = 61.0;

/// The inner margin of a card, from `views::card`'s frame. It is the one part of a card's
/// chrome that is spent *below* its body, so the plate's box has to leave it to the next
/// card; the title row above the body is already outside the space `plate_height` sees.
const CARD_PAD: f32 = 12.0;

/// A card's own chrome: 12 px margin, a ~20 px title row, 8 px under it, 12 px margin.
const CARD_CHROME: f32 = 2.0 * CARD_PAD + 20.0 + 8.0;

/// What a card spends **above** its body: the top margin, the title row and the 8 px under it.
/// The bottom margin is [`MAP_TAIL`]'s and [`CARD_CHROME`]'s, not this one's: counting it twice
/// is a page that stops 12 px short of its box.
const CARD_HEAD: f32 = CARD_PAD + 20.0 + 8.0;

/// What the column of stories is guaranteed while the page has room for it: its card and
/// one story row (the second row is what a taller window buys). A floor under the column,
/// not a share of the page.
const STORIES_MIN: f32 = ROW_HEIGHT + CARD_CHROME;

/// The gap between two blocks of the page.
const CARDS_GAP: f32 = 14.0;

/// What the map card spends under its plate: the space to the rollup, the rollup's own band,
/// the card's bottom margin and its hairline. Nothing of the page's below the card is in here.
const MAP_TAIL: f32 = theme::space::SM + PLACES_BAND + CARD_PAD + 1.0;

/// What the page spends under the plate when it draws no briefing: the map card's tail, one gap,
/// and the floor under the column of stories.
const BELOW_PLATE: f32 = MAP_TAIL + STORIES_MIN + CARDS_GAP;

/// The box a card is measured in before it is drawn, tall enough for any briefing a model writes
/// in one pass. A box, not a clip: the card is laid out whole inside it and only its height is
/// read ([`brief_height`]).
const PROBE_BOX: f32 = 4_096.0;

/// The card width at which the map's note may carry its second clause; below it the note
/// is the place count and the window only.
///
/// Measured before this bound: at 700 px the full note ("33 place(s) in the last 72 h · 0
/// coastlines · …") was a galley 10 px wider than its scissor, so the one truncated text
/// run in the whole layout matrix was here. A note that cannot be read is not a note.
/// 600 px is where the long note fits beside the card's title: ~74 px of title, 6 px of
/// gap, 24 px of margins and ~460 px of note at the caption size.
const NOTE_WIDE: f32 = 600.0;

/// Padding inside a place chip, and the gap between two of them.
const CHIP_PAD: f32 = theme::space::XS;
const CHIP_GAP: f32 = theme::space::XS;

/// The tallest a chip gets: one row of body text plus its padding.
const CHIP_HEIGHT: f32 = theme::role::BODY.size + 6.0 + 2.0 * CHIP_PAD;

/// The band the place rollup may take under the plate: two rows of chips, which is what
/// the narrowest page wraps an ordinary rollup onto (14 two-letter places).
///
/// It is a *reserve*, subtracted before the plate's share is taken, so the map can never
/// grow into the chips; when the rollup needs less, the column of stories gets the rest.
/// Whatever needs more than two rows scrolls inside this band instead of pushing the page
/// past its box.
const PLACES_BAND: f32 = 2.0 * CHIP_HEIGHT + 3.0 * CHIP_GAP;

/// The widest a chip may be, whatever the archive put in the row: a name of 500 characters
/// is drawn inside a 180 px box and clipped, not turned into a 3 000 px chip.
const CHIP_WIDTH_MAX: f32 = 180.0;

/// The narrowest a chip may be, so a single "9" is still a control a pointer can hit.
const CHIP_WIDTH_MIN: f32 = 44.0;

/// How many characters of a place name a chip carries, before the count.
///
/// The archive's own column is the reason this exists: a hand-edited row, or a model that
/// wrote a sentence into `place`, must not be able to decide the width of the page.
const CHIP_CHARS: usize = 16;

/// The page's own state: the map's outlines and camera, which outlive a frame.
pub struct Overview {
    pub map: Option<MapView>,
}

impl Overview {
    pub fn new() -> Overview {
        Overview { map: None }
    }
}

/// What the Overview draws for one frame. The page reads all of it through
/// references and copies nothing out of it, so the lists stay the caller's.
pub struct Snapshot {
    pub pins: Vec<Pin>,
    pub countries: Vec<(String, i64)>,
    pub articles: Vec<Article>,
    pub counts: Counts,
    pub hours: i64,
}

/// What a rollup row counts, and therefore which read opens it.
///
/// The two kinds are two *different groupings of the same rows*, and confusing them is the
/// bug this type exists to make impossible. A country chip is a row of
/// [`crate::data::Archive::countries`], which groups by `iso`: `USA` counts every United
/// States story, including the ones filed under a city. A place is the label
/// [`crate::data::Archive::pins`] groups by and
/// [`crate::data::Archive::place_stories`] matches — `Washington`, `Madrid`. On the shipped
/// archive, 72 h window, the `USA` chip said **105** stories and the place read for `USA`
/// held **0**: every United States story names its place, so no row's label is `USA` at all.
/// A count that opens nothing is a dead door, and the word alone cannot tell a caller which
/// read to use — `USA` is a valid word of both kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A row of `Archive::countries`: stories grouped by their `iso`.
    Country,
    /// A place's own name: the label the map's dots are grouped by.
    Place,
}

/// A rollup door: the word a chip carries, what that word *is*, and the number it counted.
///
/// Built only by [`Door::country`] / [`Door::place`], so a page cannot report a word without
/// saying which grouping counts it — that pairing is what makes the window's read the same
/// question the chip answered. The count travels with it for the same reason: the strip above
/// the list can then say `400 of 1 050` instead of printing a number the list below does not
/// hold, which is the whole promise this type exists to keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Door {
    /// The word on the chip, trimmed: an `iso` for a country, a label for a place.
    pub word: String,
    pub kind: Kind,
    /// The number the chip printed: how many stories this rollup row counts.
    pub count: i64,
}

impl Door {
    /// A country's door: the `iso` `Archive::countries` groups by, and the count it gave.
    pub fn country(iso: &str, count: i64) -> Door {
        Door {
            word: iso.trim().to_string(),
            kind: Kind::Country,
            count,
        }
    }

    /// A place's door: the label the map's pins are grouped by, and the count they gave.
    ///
    /// Nothing in the shipped page opens one *yet*: the map's merged dots are the case that
    /// wants it, and `views::map::Cluster` cannot name its members (`views/map.rs`, another
    /// file this round). The card opens this door the day it can — see `map_card` — and until
    /// then the read it selects is proved against the real archive by the shell's own tests.
    #[allow(dead_code)]
    pub fn place(label: &str, count: i64) -> Door {
        Door {
            word: label.trim().to_string(),
            kind: Kind::Place,
            count,
        }
    }

    /// Whether the door carries a word at all: an empty `iso` or label is not a place.
    fn speaks(&self) -> bool {
        !self.word.trim().is_empty()
    }
}

/// What a click on this page asked for.
///
/// Two answers, because the page has two kinds of door: a **story** (a dot on the plate, a
/// row in the list below) and a **rollup row** (a chip under the map), which counts stories
/// and therefore opens a *list*, not one row. The page does not know what a page switch is:
/// it reports what was clicked, and the window decides what that means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    /// The story a reader opened: a dot on the map, or a row in the list.
    pub story: Option<i64>,
    /// The chip a reader clicked: its word, what kind of word it is, and the number on it.
    /// One field and not three, so a word can never be reported without its kind.
    pub place: Option<Door>,
}

impl Choice {
    /// The country chip a reader clicked: the `iso` and the count the chip printed.
    pub fn at_country(&mut self, iso: &str, count: i64) {
        self.place = Some(Door::country(iso, count));
    }

    /// The place chip a reader clicked: the label and the count the chip printed.
    #[allow(dead_code)]
    pub fn at_place(&mut self, label: &str, count: i64) {
        self.place = Some(Door::place(label, count));
    }

    /// The door this frame's click opened, if it opened one.
    ///
    /// `None` for a story, for nothing, and for a door that carries no word: the last case is
    /// a chip drawn for an empty `iso`, and answering `None` keeps it from becoming a list of
    /// nothing that claims a number.
    pub fn door(&self) -> Option<Door> {
        self.place.clone().filter(Door::speaks)
    }

    /// Nothing the window can act on was clicked this frame — the common case.
    pub fn nothing(&self) -> bool {
        self.story.is_none() && self.door().is_none()
    }
}

/// Draw the page. Returns everything a reader clicked: a story, or a place.
///
/// The page fills the box it is given and scrolls its own column (see the module doc), so
/// the caller must **not** wrap it in a scroll area of its own.
pub fn show_choices(ui: &mut egui::Ui, view: &mut Overview, data: &Snapshot) -> Choice {
    show_briefed(ui, view, data, None, None)
}

/// The page with the **window's briefing** above the story column.
///
/// `ask` is the briefing for the whole window (`crate::views::brief::Ask::new`) and `queue`
/// is the request queue its action writes to. Both arrive as arguments for the same reason
/// [`Snapshot`] does: this page draws, the shell reads and writes. A bullet that cites a story
/// lands in [`Choice::story`], so a reader who presses one opens it through the same door a
/// story row uses.
///
/// The card sits between the plate and the column: the plate answers "where", the briefing
/// answers "what is there", and the column is the stories themselves — in that order.
pub fn show_briefed(
    ui: &mut egui::Ui,
    view: &mut Overview,
    data: &Snapshot,
    ask: Option<&crate::views::brief::Ask<'_>>,
    queue: Option<&std::path::Path>,
) -> Choice {
    let mut choice = Choice::default();
    heading(ui, "Overview", &subtitle(data.counts, data.articles.len(), data.hours));
    // The box under the heading: one page. Everything below is planned against it, and a page
    // whose blocks do not fit it scrolls rather than cutting one of them.
    let body = ui.available_size();
    // The card is measured at the width it will be drawn at. The first measurement is what says
    // whether the page has to scroll; when it does, the page draws a bar and the card is that
    // bar narrower, so it is measured again — a band one wrap short of its card is the cut this
    // whole file is about.
    let mut planned = fitted(body);
    if ask.is_some() {
        // A page with a card under the plate is planned around a block whose height the page
        // does not own: measured, given that height, and scrolled when the three blocks do not
        // fit the box.
        let bar = bar_width(ui);
        planned = plan(body, measure_brief(ui, ask, body.x));
        if planned.page_scrolls {
            // The page will draw a bar, and the bar narrows every card: planned on the narrowed
            // width, and only kept if the page still needs to scroll at it — a narrower card can
            // wrap its way back into the box, and then there is no bar to pay for.
            let narrowed = Vec2::new((body.x - bar).max(0.0), body.y);
            let replanned = plan(narrowed, measure_brief(ui, ask, narrowed.x));
            if replanned.page_scrolls {
                planned = replanned;
            }
        }
    }
    if planned.page_scrolls {
        // One scroll surface, and it is the page: the plate keeps the world's aspect, the card
        // keeps the lines the agent wrote, and the column keeps its floor. Nothing is clipped by
        // the plan; what the box cannot show, the wheel brings.
        //
        // `ScrollSource` is set to everything **but** `DRAG`: a drag that starts on the plate is
        // the reader moving the world, not the page. A scroll area answers a drag by default,
        // and this one is the plate's own parent — measured with the drag in
        // `tests/ui_layout`'s `a_dragged_canvas_stays_on_its_plate`: `the drag moved the picture
        // by [0.0 0.0]`, the page had swallowed the gesture and the map never panned.
        // The wheel, the bar and the keyboard still scroll the page.
        egui::ScrollArea::vertical()
            .scroll_source(egui::scroll_area::ScrollSource::MOUSE_WHEEL)
            .auto_shrink([false, false])
            .show(ui, |ui| page_body(ui, view, data, &mut choice, ask, queue, planned));
    } else {
        page_body(ui, view, data, &mut choice, ask, queue, planned);
    }
    choice
}

/// The briefing's card, measured from a throwaway draw of it.
///
/// A block whose height the page does not know is a block the page cannot give room to: the band
/// used to be a typed 160 px, and a briefing with four bullets painted its fourth line at y 578
/// in a band that ended at 582 — a severed line, and the footer never drawn at all. The card is
/// drawn once in a child `Ui` that is clipped away and **id-salted**, so its widget ids are not
/// the ids the real draw will register: a probe that shared them would register every button
/// twice, and a press on one would be answered by whichever copy egui kept.
///
/// The box is [`PROBE_BOX`] tall, not a clip: the card is laid out whole and its own height is
/// the answer. `queue` is `None` because a probe cannot be pressed.
fn measure_brief(
    ui: &mut egui::Ui,
    ask: Option<&crate::views::brief::Ask<'_>>,
    width: f32,
) -> f32 {
    let Some(ask) = ask else {
        return 0.0;
    };
    let mut probe = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("overview-briefing-measure")
            .max_rect(egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(width.max(0.0), PROBE_BOX),
            )),
    );
    probe.set_clip_rect(egui::Rect::NOTHING);
    // Disabled as well as clipped: a probe that registered live widgets would put a second copy
    // of every bullet and every button in egui's interactive inventory, where a debug build
    // paints it and a real one could still reach it by keyboard.
    probe.disable();
    crate::views::brief::show(&mut probe, ask, None);
    probe.min_rect().height().max(0.0)
}

/// The width a scrolling page loses to its bar: the bar itself plus the content margins the
/// scroll area insets its content by, read from the style that draws them rather than typed
/// here. Measured in the frame: a 1 052 px page drew its cards 1 040 px wide, 12 px narrower.
fn bar_width(ui: &egui::Ui) -> f32 {
    ui.spacing().scroll.allocated_width()
}

/// One frame's sizes for the page, decided before anything is drawn.
///
/// The three heights and the two scroll decisions are one value because they are one plan: the
/// plate's height decides what is left, the card's height spends it, the column takes the rest —
/// and either the page fits, or it scrolls, or the card alone is taller than a page and scrolls
/// inside its band.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// The plate's height: the card's inner width at the world's aspect ([`plate_height`]).
    plate: f32,
    /// The briefing's band: the height the card measured, up to one page box.
    band: f32,
    /// The stories column's height, or `None` for "whatever the page has left": a page that
    /// fits hands its slack to the column rather than typing it, so the column ends where the
    /// box does and a 12 px disagreement between the chrome's sum and the chrome's drawing
    /// cannot leave a dead band at the page's foot.
    column: Option<f32>,
    /// The card is taller than one page box, so its band is a viewport it scrolls in. Every
    /// line stays reachable; the alternative — the typed band — threw them away.
    card_scrolls: bool,
    /// The page is taller than the box it was given, so the page scrolls.
    page_scrolls: bool,
}

/// What the page does with the box under its heading, and with the card's measured height.
///
/// The plate is **not** a share of the height any more: a plate shorter than its width at the
/// world's aspect crops the poles off the world (at 1280×800 the plan used to give it 209 px of
/// the 489 its 1 028 px width asks for, and at 900×620 203 of 368). The aspect decides, the card
/// below it and the column under that are what the page must find room for — and when the box
/// cannot hold them, the page scrolls.
///
/// The band's ceiling is one page box: a card taller than the screen is not a briefing any more,
/// it is the page, so it keeps a band of one page and scrolls inside it. Every realistic card —
/// a sentence, a handful of bullets, a footer — is under that, so it is drawn whole.
fn plan(body: Vec2, wanted: f32) -> Plan {
    // The aspect, not a share of the box and not a ceiling: a plate shorter than its width at
    // the world's aspect is a map with its poles cut off. A box too short for it scrolls.
    let inner = (body.x - 2.0 * CARD_PAD).max(0.0);
    let plate = (inner / PLATE_ASPECT).max(MIN_PLATE);
    let band = wanted.min(body.y.max(0.0));
    // What the page is, before the box decides how much of it is on screen: the card at the
    // plate's aspect, the card the agent wrote, the column's floor.
    let natural = CARD_HEAD
        + plate
        + BELOW_PLATE
        + if band > 0.0 { CARDS_GAP + band } else { 0.0 };
    let scrolls = body.y < natural - 0.5;
    Plan {
        plate,
        band,
        // A page that scrolls has no "rest" to measure: the content is taller than the box, so
        // the column keeps its floor and the page carries the overflow.
        column: scrolls.then_some(STORIES_MIN),
        card_scrolls: wanted > band + 0.5,
        page_scrolls: scrolls,
    }
}

/// The plan for a page that draws **no** briefing's card: the box is the page.
///
/// With no card under the plate there is nothing whose height the page does not own, so the page
/// can fill its box exactly: the plate takes the world's aspect where the box has room for it and
/// gives height up until the column's floor is paid for ([`BELOW_PLATE`]), and the column takes
/// the rest. This is the plan the callers that ask for the map and the stories only get
/// ([`show_choices`]), and the one `tests/overview.rs` measures.
fn fitted(body: Vec2) -> Plan {
    let inner = (body.x - 2.0 * CARD_PAD).max(0.0);
    let ceiling = (body.y - CARD_HEAD - BELOW_PLATE)
        .max(MIN_PLATE)
        .min(body.y.max(0.0))
        .min(MAX_PLATE);
    let plate = plate_height(Vec2::new(inner, ceiling));
    Plan {
        plate,
        band: 0.0,
        column: None,
        card_scrolls: false,
        page_scrolls: false,
    }
}

/// The page as the plan decided it: the map card at the plate's height, the briefing's card in a
/// band as tall as the card measured, then the column of stories with the room that is left.
fn page_body(
    ui: &mut egui::Ui,
    view: &mut Overview,
    data: &Snapshot,
    choice: &mut Choice,
    ask: Option<&crate::views::brief::Ask<'_>>,
    queue: Option<&std::path::Path>,
    plan: Plan,
) {
    map_card(ui, view, data, choice, plan.plate);
    if let Some(ask) = ask {
        if plan.band > 0.0 {
            ui.add_space(CARDS_GAP);
            let opened = in_band(ui, plan.band, |ui| {
                if plan.card_scrolls {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| crate::views::brief::show(ui, ask, queue))
                        .inner
                } else {
                    crate::views::brief::show(ui, ask, queue)
                }
            });
            if let Some(story) = opened {
                // One story per press: a briefing's bullet is a door to the story behind it, and
                // a place the reader picked on the plate stays picked.
                choice.story = Some(story);
            }
        }
    }
    // The column is the page's last block and is given exactly the room that is left: its rows
    // scroll inside the card, so the card ends where the page does whatever the plan above
    // spent. It is never dropped while there is a pixel for it — the stories are what the page
    // is for, and a clipped card is a card whose rows are one wheel away, while an absent card
    // is a dead end.
    ui.add_space(CARDS_GAP);
    let column = plan
        .column
        .unwrap_or_else(|| ui.available_height().max(0.0));
    in_band(ui, column, |ui| stories_card(ui, data, choice));
}

/// Runs `body` in a box `height` tall, starting where the page's cursor is, and clips what it
/// paints to that box.
///
/// A card is a box: its rows scroll inside it, its frame ends where the box does, and nothing it
/// paints may stand below the page. `available_height` alone was not enough to guarantee that —
/// measured on the real page at a 1 092×761 panel, the story card's frame came out 128 px tall
/// in 80 px of room, and the extra 48 px were painted over the content box's edge — because a
/// frame's content asks egui for the room the *page* has left, not the room its own box has.
/// Handing the block a rect and a clip is the one spelling of "this is your room" that both the
/// layout and the painter obey.
fn in_band<R>(ui: &mut egui::Ui, height: f32, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let room = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width(), height.max(0.0)),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(room), |ui| {
        // Cut the clip that is already there (the page's scroll viewport), never replace it: a
        // block that reset it would paint its rows over the status row below the page.
        ui.set_clip_rect(room.intersect(ui.clip_rect()));
        body(ui)
    })
    .inner
}

/// The page as the window calls it **today**: the story a reader opened, and nothing else.
///
/// This exists for one caller in another file (`app.rs`), which still reads a story id out
/// of this page; that file is being edited by someone else and a signature change there is
/// theirs to make. It is [`show_choices`]`.story` and nothing else — one implementation of
/// the page, two ways to read it — so the day the window calls [`show_choices`] this
/// function is deleted, and until then **a click on a place is drawn and reported by the
/// page but nobody hears it**: the rollup is exactly as inert through this door as it was
/// before the page could answer at all.
pub fn show(ui: &mut egui::Ui, view: &mut Overview, data: &Snapshot) -> Option<i64> {
    show_choices(ui, view, data).story
}


/// The height of the plate, from the room it is offered.
///
/// `room` is the card's *inner* space — the card's own margins and title row are already
/// spent by the time this is asked (egui knows them; a constant here would be a second
/// answer to the same question).
///
/// **The world's aspect decides**, in the width the card gives the plate. A plate shorter than
/// that crops the poles off the map: at 1280×800 a plan that shared the height out gave the
/// plate 209 px of the 489 its 1 028 px width asks, at 900×620 203 of 368, and every place north
/// of 52.5°N fell outside the picture. The floor — 360 px of width ([`MIN_PLATE`]) — is the one
/// number here that is typed, and a box too short to hold even it gets the box, so a page that
/// small still paints.
///
/// The page does not shrink the plate to make room for the cards below it; it scrolls instead
/// ([`show_briefed`]), because a cropped world is a wrong map and a scrolled page is a long one.
/// There is no ceiling on the height either: the plate used to stop at 762 px, which on a
/// 2 400×1 400 window meant a 2 136 px-wide card showing 2.8:1 of world — the same crop of the
/// poles the ceiling was meant to prevent.
pub fn plate_height(room: Vec2) -> f32 {
    (room.x / PLATE_ASPECT)
        .max(MIN_PLATE)
        .min(room.y.max(0.0))
}

/// The widget id of one story's row: stable across frames and across a scroll, so a click
/// lands on the row it was aimed at and a test (or the app) can find the row without
/// sweeping pixels.
pub fn story_id(article: i64) -> Id {
    Id::unique(("overview-story", article))
}

/// The widget id of one place's chip, keyed by the place itself: two frames, a scroll and
/// a re-read all spell a place the same way, so the id survives them.
pub fn place_id(place: &str) -> Id {
    Id::unique(("overview-place", place))
}

/// The map card: the plate at the height the page planned, then the place rollup.
///
/// Built here rather than by `views::map::card` because three of this page's decisions
/// belong to this card and not to the map: the plate's height comes from the page's room
/// (see [`plate_height`]), the note has to shorten before it is clipped at 700 px, and the
/// place rows have to be clickable. The map itself is unchanged — the same
/// [`MapView::show`], the same dots, the same hit test.
fn map_card(
    ui: &mut egui::Ui,
    view: &mut Overview,
    data: &Snapshot,
    choice: &mut Choice,
    height: f32,
) {
    let note = map_note(ui.available_width(), data, view);
    card(ui, "World map", &note, |ui| {
        if data.pins.is_empty() {
            // An empty part of a page says what the reader can do about it, not only what is
            // missing: the two things that fill this card are collecting a story with a place
            // and widening the window in the rail.
            ui.label(theme::dim(
                "Nothing collected in this window carries a coordinate yet. A story lands on \
                 the map once it names a place — Refresh, or widen the window in the rail, to \
                 look for one.",
            ));
        } else {
            // The plate's height comes from the page, not from the room this card happens to
            // have: the card's chrome, the rollup under it and the column below are the page's
            // business ([`plan`]), and a card that sized its own plate is how the map came to be
            // a strip.
            let map = view
                .map
                .get_or_insert_with(|| MapView::new(&crate::assets::geojson()));
            // A click on a dot is a click on the news filed there: the map answers with
            // the pin it hit, the pin carries the newest story at that place, and that is the
            // story the reading pane opens (`tests/e2e_map_click.rs` pins it: the dot opens
            // the newest story and not the older one).
            //
            // A dot that stands for **several places** is a door onto several lists: `Cluster`
            // carries `members`, and the card draws them as chips below (`merged_places`), each
            // its own `Choice::at_place` press. The dot itself still answers its representative
            // pin, so a plain press opens the newest story as it always did.
            //
            // The plate draws the countries too, one ring per country a story names, so a
            // story filed in one place still has a door in every other place it is about; a
            // press on a ring answers the same `Pick::Country` a press on that country's land
            // answers, which is the chip's read (`tests/map_countries.rs`).
            if let Some(index) = map.show_countries(ui, &data.pins, &data.countries, height) {
                if let Some(pin) = data.pins.get(index) {
                    if pin.article > 0 {
                        choice.story = Some(pin.article);
                    }
                }
            }
            // The press's whole answer, not only the story: a dot that merges places, and
            // the land under a press that hit no dot. `views/map.rs::Pick` carries the three,
            // and each becomes the read that answers it — a country is the chip's own read
            // (`Choice::at_country`), a merged dot's places are `Door::place` rows, and a dot
            // stays the story above.
            if map.pressed() {
                match map.picked() {
                    Some(Pick::Country(iso)) => {
                        let count = data
                            .countries
                            .iter()
                            .find(|(code, _)| code == iso)
                            .map(|(_, count)| *count)
                            .unwrap_or(0);
                        choice.at_country(iso, count);
                    }
                    // The merged dot's list is drawn below rather than acted on: it stays on
                    // the card while the reader chooses from it, and each chip is its own
                    // press.
                    _ => {}
                }
            }
            if let Some(Pick::Places(members)) = map.picked() {
                merged_places(ui, data, members, choice);
            }
        }
        places(ui, data, choice);
    })
}

/// The places a merged dot stands for, as the doors they are.
///
/// A dot that merges places is a door onto several lists, and `Choice` can carry one `Door` at
/// a time — so the card draws the dot's members as the same chips the country rollup uses, and
/// a press on one opens that place's stories (`Door::place` → `Archive::place_stories`). This
/// is the half of `tests/map_audit.rs`' complaint that page furniture can fix: China and
/// Beijing sit on one coordinate at every zoom, so the dot can never split, and the reader who
/// pressed it asked for the news, not for one story.
///
/// Nothing is drawn for a dot of one place: the story it opens is already the press's answer.
fn merged_places(ui: &mut egui::Ui, data: &Snapshot, members: &[usize], choice: &mut Choice) {
    let pins: Vec<&Pin> = members.iter().filter_map(|index| data.pins.get(*index)).collect();
    if pins.len() < 2 {
        return;
    }
    ui.add_space(theme::space::XS);
    ui.label(theme::dim(&format!(
        "this dot stands for {} places:",
        nf(pins.len() as i64)
    )));
    ui.horizontal_wrapped(|ui| {
        for pin in pins {
            if place_chip(ui, &pin.place, pin.count).clicked() {
                choice.at_place(&pin.place, pin.count);
            }
        }
    });
}

/// The card's note: how many places the window holds, over which span, and — only when
/// the card is wide enough to draw it whole — what the outlines cost to read.
///
/// The outline clause is the one that goes, not the place count: the count and the window
/// are what the card is *about*, and the cost of the read is a fact for a reader who has
/// room for it (`tests/smoke.rs` reads the same two numbers back out of this note, so the
/// shapes on the plate and the count on the card come from one read of one file).
fn map_note(width: f32, data: &Snapshot, view: &mut Overview) -> String {
    let note = format!(
        "{} in the last {}",
        plural(data.pins.len() as i64, "place", "places"),
        window_words(data.hours)
    );
    if width < NOTE_WIDE || data.pins.is_empty() {
        return note;
    }
    let map = view
        .map
        .get_or_insert_with(|| MapView::new(&crate::assets::geojson()));
    format!("{note} · {}", map.outlines())
}

/// The country rollup: one chip per country, each a door into that country's news.
///
/// **The chips are countries**, because that is what [`Snapshot::countries`] is: rows of
/// [`crate::data::Archive::countries`], grouped by `iso`. The door says so
/// ([`Choice::at_country`]), and the window reads the matching list
/// ([`crate::data::Archive::country_stories`]). Before this, the chip reported its word as a
/// *place* and the window read the place label, so on the shipped archive the `USA` chip
/// counted 105 stories and the list it opened held 0 — the "I pick a filter and simply
/// nothing comes" report, measured by `tests/map_audit.rs`.
///
/// The band is capped at [`PLACES_BAND`] and scrolls inside it, because the number of
/// countries and their names are the archive's to decide — a rollup that grew with its
/// content would push the stories off the page, which is the defect this page was reported
/// for.
fn places(ui: &mut egui::Ui, data: &Snapshot, choice: &mut Choice) {
    // **A chip is a promise**: it prints a number and opens the list of exactly that many rows.
    // A rollup row that counts nothing has no list to open, so it is not drawn — a chip reading
    // `NO 0` is a door onto nothing, and a reader who presses it learns less than the sentence
    // below says. [`crate::data::Archive::countries`] cannot produce such a row (`count(*)`
    // over the window), so this keeps a caller's list honest rather than fixing a read.
    let offers: Vec<&(String, i64)> = data
        .countries
        .iter()
        .filter(|(_, count)| *count > 0)
        .collect();
    if offers.is_empty() {
        ui.label(theme::dim(
            "No country has a story in this window yet. Refresh, or widen the window in the \
             rail: a chip appears here for each country a story names.",
        ));
        return;
    }
    ui.add_space(theme::space::SM);
    egui::ScrollArea::vertical()
        .id_salt("overview-places")
        .max_height(PLACES_BAND)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (iso, count) in offers {
                    if place_chip(ui, iso, *count).clicked() {
                        // A *country*, not a place: the chip's number is
                        // `Archive::countries`' count, so the list has to be the read that
                        // groups the same way. The chip cannot claim a story it cannot open,
                        // because both sides of that promise are the same grouping.
                        choice.at_country(iso, *count);
                    }
                }
            });
        });
}

/// One place as a chip: the name the archive gave it and how many stories are filed
/// there, in a control a pointer and a keyboard can both use.
///
/// The width is bounded twice — the label is cut to [`CHIP_CHARS`] before it is shaped,
/// and the box is clamped to [`CHIP_WIDTH_MAX`] and painted under a clip — so a hostile
/// place name cannot decide the width of the card. Measured cases in `tests/overview.rs`:
/// 500 characters, an empty name, `1234`, a single emoji.
fn place_chip(ui: &mut egui::Ui, place: &str, count: i64) -> egui::Response {
    let font = FontId::proportional(theme::role::BODY.size);
    let label = chip_label(place, count);
    let text = ui
        .ctx()
        .fonts_mut(|fonts| fonts.layout_no_wrap(label.clone(), font.clone(), theme::INK));
    let width = (text.size().x + 2.0 * CHIP_PAD).clamp(CHIP_WIDTH_MIN, CHIP_WIDTH_MAX);
    // One widget on one rect (`allocate_space` makes the space and no widget), the same
    // shape the topics rows use: `Sense::click` is what makes it Tab-focusable, and the
    // ring is painted inside it below.
    let (_auto, rect) = ui.allocate_space(Vec2::new(width, CHIP_HEIGHT));
    let response = ui
        .interact(rect, place_id(place), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));

    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter().with_clip_rect(rect);
    if response.hovered() || response.has_focus() {
        painter.rect_filled(rect, egui::CornerRadius::same(theme::RADIUS), theme::RAISED);
    }
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(theme::RADIUS),
        theme::line(),
        egui::StrokeKind::Inside,
    );
    // The label is drawn inside the clip, so a name longer than the box is cut by the
    // painter rather than widening the chip: the same text, one box, whatever the archive.
    let inner = rect.shrink2(Vec2::new(CHIP_PAD, CHIP_PAD));
    let galley = line(ui, &label, font, theme::INK, inner.width());
    painter.galley(
        Pos2::new(inner.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        theme::INK,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(theme::RADIUS),
            theme::focus_stroke(),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// The words on a chip: the archive's place name, bounded, then the count.
fn chip_label(place: &str, count: i64) -> String {
    // One line, always. A newline in the archive's place column would otherwise become a
    // second row inside a chip drawn for one (`LayoutJob` breaks on newlines by default),
    // and a chip two rows tall would push the page's own sum apart by a number nobody
    // measured. Control characters become spaces, then the name is cut to [`CHIP_CHARS`].
    let flat: String = place
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let name = super::clip(&flat, CHIP_CHARS);
    if name.trim().is_empty() {
        // An empty place is still a place the archive filed stories under: the chip says
        // so rather than becoming a blank control nobody can aim at.
        return format!("unknown place {}", nf(count));
    }
    format!("{name} {}", nf(count))
}

/// The stories card: one row per story, in a column that scrolls inside itself.
fn stories_card(ui: &mut egui::Ui, data: &Snapshot, choice: &mut Choice) {
    let note = format!("the last {}, newest first", window_words(data.hours));
    card(ui, "Stories", &note, |ui| {
        if data.articles.is_empty() {
            // The two actions a reader has from here, and the only two: re-read the archive
            // (`Refresh` in the rail) or look further back (the window chips beside it).
            ui.label(theme::dim(
                "No stories in this window yet. Refresh, or widen the window in the rail.",
            ));
            return;
        }
        // The column's own room, measured: the page's height is the heading plus the two
        // cards, and this is what the second one gets. `auto_shrink([false, false])` is
        // what makes the card end where the page does instead of hugging its rows.
        let room = ui.available_height().max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("overview-stories")
            .max_height(room)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for article in &data.articles {
                    let (response, link) = story_row(ui, article);
                    // A press on the row's source link is the link's: the row suppresses its
                    // own click while the pointer is over that cell, so one press opens one
                    // thing (the Reader) and never the reading pane as well.
                    if response.clicked() && !crate::views::reader::inside(ui, link) {
                        choice.story = Some(article.id);
                    }
                }
            });
    });
}

// ---------------------------------------------------------------------- words

/// The heading's sentence: the window's own count first, then what the archive holds, in plain
/// words. The totals behind it are the whole archive — entities and connections included — which
/// is why the sentence says so: the cards under it are the window.
fn subtitle(counts: Counts, listed: usize, hours: i64) -> String {
    // The first clause is **the list in the window**, not the archive: a reader who presses
    // the rail's `24 h` chip watches this line, and an archive-wide count does not move when the
    // window does — measured before this change: `3 stories` at 24 h, at 72 h and after
    // Refresh, while the list read 2, 3 and 1 rows. The archive's own size is still said, once,
    // in the clauses after it, because that is what the map and the graph are drawn from.
    format!(
        "{} in {} · {} · {} in the archive",
        plural(listed as i64, "story", "stories"),
        window_words(hours),
        plural(counts.entities, "entity", "entities"),
        plural(counts.links, "connection", "connections"),
    )
}

/// `1 story`, `0 stories`, `1 204 stories`: a count a reader can say out loud.
fn plural(count: i64, one: &str, many: &str) -> String {
    format!("{} {}", nf(count), if count == 1 { one } else { many })
}

/// `72` -> `three days`. The rail's window in words, because a note is read and
/// `72 h` is decoded.
fn window_words(hours: i64) -> String {
    match hours {
        24 => "a day".to_string(),
        72 => "three days".to_string(),
        168 => "seven days".to_string(),
        other => format!("{other} hours"),
    }
}

/// One line of the story's own words: the summary the archive kept, or the body
/// when there is no summary. Never the title again, and nothing invented.
fn lede(article: &Article) -> &str {
    let summary = article.summary.trim();
    if summary.is_empty() {
        article.content.trim()
    } else {
        summary
    }
}

/// Who filed it, and where it says it happened — each part only when the archive
/// has it, so a row never wears a separator with nothing after it.
fn meta_text(article: &Article) -> String {
    let source = article.source.trim();
    let source = if source.is_empty() { "unknown source" } else { source };
    let country = article.country.trim();
    if country.is_empty() {
        source.to_string()
    } else {
        format!("{source} · {country}")
    }
}

// ----------------------------------------------------------------------- rows

/// One story as a row: the title with the time beside it, the source, one line of
/// the lede, and — at the right of those two lines — the Reader's source link.
///
/// The height is measured from the fonts the row is drawn in, so the type scale
/// cannot drift away from the row that holds it, and a row that is scrolled out of
/// the clip rect is skipped before any text is laid out.
///
/// Answers the row's own response **and** the link's cell: a row is a door to the reading pane
/// and its link is a door to the Reader, they overlap, and the caller needs both rectangles to
/// keep one press from being two doors (`reader::inside`).
fn story_row(ui: &mut egui::Ui, article: &Article) -> (egui::Response, egui::Rect) {
    // The three fonts are the role table's, not numbers typed here: a headline is body text,
    // the source-and-country line is secondary, and the time is a caption. The design audit
    // (`tests/design_audit.rs`) reads every painted (size, face) pair and refuses one no role
    // produces — a literal `proportional(12.0)` passes that by luck today and stops being on
    // the ladder the day the ladder moves, which is exactly what it did to the time.
    let title_font = theme::role::BODY.font();
    let small_font = theme::role::SECONDARY.font();
    let stamp_font = theme::role::CAPTION.font();
    // 0.37 moved the shaping queries onto `&mut FontsView`, so the metrics are asked on
    // the context rather than through `ui.fonts`.
    let (h_title, h_small, h_stamp) = ui.ctx().fonts_mut(|fonts| {
        (
            fonts.row_height(&title_font),
            fonts.row_height(&small_font),
            fonts.row_height(&stamp_font),
        )
    });
    // The row's own padding: its share of the inset a reader sees.
    //
    // Eight, so the number the design audit measures — the **sum** of everything between a
    // painted surface's edge and the column of text inside it — is 20 px, five units of
    // `theme::space::UNIT`. That sum is the card's 12 px (`views::card` paints `space::MD`
    // less the hairline and counts the hairline back as the stroke: 11 + 1) plus this. It read
    // **18** with six here, which the audit refuses. Six was also a step short of [`ROW_HEIGHT`],
    // the 61 px the page's plan reserves per story: that constant states eight, so the plan and
    // the row disagreed by four pixels a row and the column was reserved short.
    const PAD: f32 = 8.0;
    const GAP: f32 = 3.0;
    let height = PAD + h_title + GAP + h_small + GAP + h_small + PAD;
    let width = ui.available_width();
    let (_auto, rect) = ui.allocate_space(Vec2::new(width, height));
    // Keyed by the story, not by the frame: a row scrolled out of the column and back is
    // the same widget, so a click still lands where the reader aimed and a test can ask
    // for the row by id (`Context::read_response`) instead of sweeping pixels.
    let response = ui
        .interact(rect, story_id(article.id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, article.title.trim())
    });

    if !ui.is_rect_visible(rect) {
        // Nothing is painted, so there is no link either: `Rect::NOTHING` answers `false` for
        // every point, which is what the caller asks before it treats a press as its own.
        return (response, egui::Rect::NOTHING);
    }

    let painter = ui.painter().with_clip_rect(rect);
    if response.hovered() || response.has_focus() {
        painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::RAISED);
    }
    // a hairline above every row: this is a list, not a paragraph
    painter.hline(rect.x_range(), rect.top() + 0.5, theme::line());

    let stamp = when(&article.published);
    // The time is a caption, in the caption's own ink. It used to be `monospace(12.0)` in
    // `FAINT`, which the design audit measured at **3.73:1** on the card's `PANEL` — the
    // worst pair in the window, and under the 4.5:1 floor for text. `FAINT` is a *stroke*
    // colour (`src/theme.rs`: a boundary needs 3:1); text it cannot carry. The caption role
    // is 11 px in `DIM`, 6.85:1 here.
    let time = line(
        ui,
        &stamp,
        stamp_font,
        theme::role::CAPTION.colour,
        f32::INFINITY,
    );
    let link_w = crate::views::reader::source_link_width(ui);
    let lower_width = (rect.width() - PAD * 2.0 - link_w - GAP).max(80.0);
    let text_width =
        lower_width.min((rect.width() - PAD * 2.0 - time.size().x - 12.0).max(80.0));
    let title = line(ui, article.title.trim(), title_font.clone(), theme::INK, text_width);
    let meta = line(
        ui,
        &meta_text(article),
        small_font.clone(),
        theme::role::SECONDARY.colour,
        lower_width,
    );
    let body = lede(article);
    let lede_text = if body.is_empty() {
        "No text was stored for this story."
    } else {
        body
    };
    let lede = line(ui, lede_text, small_font, theme::DIM, lower_width);

    let left = rect.left() + PAD;
    let mut y = rect.top() + PAD;
    painter.galley(Pos2::new(left, y), title, theme::INK);
    painter.galley(
        Pos2::new(
            rect.right() - PAD - time.size().x,
            y + (h_title - h_stamp) * 0.5,
        ),
        time,
        theme::role::CAPTION.colour,
    );
    y += h_title + GAP;
    painter.galley(
        Pos2::new(left, y),
        meta,
        theme::role::SECONDARY.colour,
    );
    y += h_small + GAP;
    painter.galley(Pos2::new(left, y), lede, theme::DIM);

    // The row's second question — "what does the page itself say?" — is a link, and it is a
    // widget rather than paint: it takes a press and a keyboard focus of its own, and its cell
    // is measured from the caption it is drawn in (`reader::source_link_width`).
    //
    // The cell ends at the row's own bottom edge rather than at the last line, because egui
    // moves a vertical layout's cursor to `widget_rect.max.y + item_spacing` when a widget is
    // placed with `Ui::put` (`layout.rs`): a cell that stopped short of the row would pull the
    // next row up over this one by that much. Ending on the row's bottom, the cursor lands
    // exactly where the row's own allocation already left it.
    let link_rect = egui::Rect::from_min_max(
        Pos2::new(rect.right() - PAD - link_w, y - h_small - GAP),
        Pos2::new(rect.right() - PAD, rect.bottom()),
    );
    let _pressed = crate::views::reader::source_link_in(ui, &article.url, link_rect);

    // The focus ring is painted last and inside the row, so the keyboard's place is
    // never covered by the text it is on.
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(0.5),
            egui::CornerRadius::ZERO,
            Stroke::new(1.0_f32, theme::INK),
            egui::StrokeKind::Inside,
        );
    }
    (response, link_rect)
}

/// The first `cap` characters of `text`, cut on a character boundary.
///
/// The unit a one-line row is built from: a row shows an elided single line, so no
/// character past the point where the line runs out can ever appear in it, and taking
/// the prefix is what keeps a stored string of any length from becoming a layout job of
/// the same length. It never allocates and never cuts through a multi-byte character.
pub fn head(text: &str, cap: usize) -> &str {
    match text.char_indices().nth(cap) {
        Some((byte, _)) => &text[..byte],
        None => text,
    }
}

/// How many characters of a stored string a one-line job in `width` pixels can use.
///
/// Two facts make this a bound and not a guess. A single line is filled to `width` and
/// then elided (`TextWrapping { max_rows: 1, break_anywhere: true, overflow_character }`),
/// so a character past the point where `width` runs out can never be drawn. And the
/// narrowest advance in the font is what that point is measured with, doubled and padded:
/// kerning, a zero-width character or a fallback font can pack more characters into the
/// line than the estimate, and the prefix only has to be *longer* than the line can hold
/// — then the ellipsis lands exactly where it landed when the whole string was shaped,
/// and an ordinary title is drawn unchanged. Measured cost of not having it, on this page:
/// 135–145 ms per frame for 400 rows of a 100 000-character title, against 1–2 ms for the
/// same 400 rows of ordinary text.
fn budget(ui: &egui::Ui, font: &FontId, width: f32) -> usize {
    if !width.is_finite() {
        // A line with no width limit (the clock beside a title) is a short one: a stamp
        // is at most 25 characters once `when` has bounded it, and this is room to spare
        // rather than a second rule about what a stamp is.
        return 96;
    }
    let narrowest = ui.ctx().fonts_mut(|fonts| {
        fonts
            .glyph_width(font, 'i')
            .min(fonts.glyph_width(font, '.'))
            .min(fonts.glyph_width(font, ' '))
    });
    let narrowest = if narrowest.is_finite() && narrowest > 0.0 { narrowest } else { 1.0 };
    let fits = (width / narrowest).ceil().max(0.0) as usize;
    fits.saturating_mul(2).saturating_add(16)
}

/// One line of text, elided with an ellipsis instead of wrapped: a row in a list is
/// one line however wide the page is.
///
/// The job is handed the prefix of `text` that can still be drawn and nothing else — see
/// [`budget`]. `max_rows: 1` truncates the *rows*, not the work: `epaint`'s `layout`
/// shapes the whole string first, so before this bound a row of a 100 000-character title
/// shaped 100 000 glyphs on every frame (measured through this page: 400 such rows cost
/// 135–145 ms per frame against 1–2 ms for 400 ordinary ones).
fn line(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let cap = budget(ui, &font, width);
    let mut job = egui::text::LayoutJob::simple_singleline(head(text, cap).to_owned(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story(title: &str, summary: &str, content: &str) -> Article {
        Article { title: title.to_string(), summary: summary.to_string(), content: content.to_string(), ..Article::default() }
    }

    #[test]
    fn a_window_is_named_in_words() {
        assert_eq!(window_words(24), "a day");
        assert_eq!(window_words(72), "three days");
        assert_eq!(window_words(168), "seven days");
        // anything else still reads, and still says what it is
        assert_eq!(window_words(48), "48 hours");
    }

    #[test]
    fn a_count_of_one_is_not_a_count_of_many() {
        assert_eq!(plural(0, "story", "stories"), "0 stories");
        assert_eq!(plural(1, "story", "stories"), "1 story");
        assert_eq!(plural(1204, "story", "stories"), "1 204 stories");
    }

    #[test]
    fn the_heading_says_the_windows_count_first_and_then_the_archive() {
        let counts = Counts { relevant: 1204, entities: 318, links: 902, ..Counts::default() };
        // The first clause is the list in the window; the archive's size follows it.
        assert_eq!(
            subtitle(counts, 3, 24),
            "3 stories in a day · 318 entities · 902 connections in the archive"
        );
        assert_eq!(
            subtitle(counts, 1, 168),
            "1 story in seven days · 318 entities · 902 connections in the archive"
        );
        // The count the chip press has to move: the same archive, a narrower window, fewer rows.
        assert_ne!(subtitle(counts, 3, 72), subtitle(counts, 1, 24));
        // an empty archive says so in the same words, not in a special case
        assert_eq!(
            subtitle(Counts::default(), 0, 72),
            "0 stories in three days · 0 entities · 0 connections in the archive"
        );
    }

    #[test]
    fn the_lede_is_the_summary_and_never_the_title_again() {
        assert_eq!(
            lede(&story("A title", "The words the archive kept.", "The body.")),
            "The words the archive kept."
        );
        // no summary: the body is the story's own words
        assert_eq!(lede(&story("A title", "  ", "The body.")), "The body.");
        // neither: nothing, which the row says out loud
        assert_eq!(lede(&story("A title", "", "   ")), "");
    }

    #[test]
    fn the_source_line_carries_only_what_the_archive_has() {
        let mut article = story("A title", "", "");
        article.source = "The Nordic Post".to_string();
        article.country = "SE".to_string();
        assert_eq!(meta_text(&article), "The Nordic Post · SE");
        // no place: no separator with nothing behind it
        article.country.clear();
        assert_eq!(meta_text(&article), "The Nordic Post");
        // no source either, and the row still tells the truth
        article.source.clear();
        assert_eq!(meta_text(&article), "unknown source");
    }

    fn window() -> Snapshot {
        Snapshot {
            pins: Vec::new(),
            countries: Vec::new(),
            articles: Vec::new(),
            counts: Counts::default(),
            hours: 72,
        }
    }

    /// The defect this closes, measured: a typed 470 px made the plate overflow every
    /// window shorter than ~620 px (at 700×520, 328 of 470 were inside the page and the
    /// plate's caption was painted at y = 586 in a 520 px window) and left 35 % of a
    /// 2400×1400 page as map with ~860 px of nothing under it.
    #[test]
    fn the_plate_is_its_own_aspect_and_never_a_typed_height() {
        // The page boxes of the audit, less the heading, and the card's inner width —
        // the five sizes the layout was measured at.
        for (width, room) in [
            (449.0_f32, 402.0_f32),
            (649.0, 502.0),
            (1029.0, 702.0),
            (1429.0, 1082.0),
            (2149.0, 1282.0),
        ] {
            let height = plate_height(Vec2::new(width, room));
            assert!(height >= MIN_PLATE, "{width}×{room} gave {height}");
            assert!(
                height <= width / PLATE_ASPECT + 0.01,
                "{width}×{room} gave {height}, taller than the world's aspect"
            );
            assert!(
                height <= room - PLACES_BAND,
                "the plate would take the rollup's band: {height} of {room}"
            );
            assert!(
                height + PLACES_BAND + STORIES_MIN <= room + MIN_PLATE,
                "the page cannot fit a plate and a column: {height} of {room}"
            );
        }
        // the floor is real, and the aspect decides on a wide, short box
        assert!(
            plate_height(Vec2::new(300.0, 120.0)) <= 120.0,
            "a page under the floor's height must fit the floor's box, not overflow it"
        );
        // No ceiling on the height: 4 000 px of card is 1 904 px of plate, or the world is
        // cropped. The page scrolls under it ([`show_briefed`]).
        assert_eq!(
            plate_height(Vec2::new(4_000.0, 4_000.0)),
            4_000.0 / PLATE_ASPECT
        );
        let short = plate_height(Vec2::new(420.0, 2_000.0));
        assert!((short - 420.0 / PLATE_ASPECT).abs() < 0.01, "{short}");
        // and it grows with the window until the box is taller than the aspect, which is the
        // other half of the defect: the map used to be the same 470 px on every taller window
        let sizes: Vec<f32> = [400.0, 600.0, 800.0, 1_000.0, 1_282.0, 4_000.0]
            .iter()
            .map(|room| plate_height(Vec2::new(2_149.0, *room)))
            .collect();
        assert!(
            sizes.windows(2).all(|pair| pair[0] <= pair[1]),
            "{sizes:?}"
        );
        assert!(
            sizes[0] < sizes[3] && (sizes[5] - 2_149.0 / PLATE_ASPECT).abs() < 0.01,
            "the plate must grow and then stop at the world's aspect: {sizes:?}"
        );
    }

    /// Measured before: at 700 px the note was a galley 10 px wider than its scissor
    /// ("33 place(s) in the last 72 h · 0 c…"). The hint is the clause that goes.
    #[test]
    fn the_note_loses_its_hint_before_it_loses_a_character() {
        let mut data = window();
        data.pins = vec![Pin {
            place: "Stockholm".to_string(),
            lat: 59.3,
            lon: 18.1,
            count: 33,
            article: 1,
        }];
        let mut view = Overview::new();
        let narrow = map_note(400.0, &data, &mut view);
        assert_eq!(narrow, "1 place in the last three days");
        let wide = map_note(1_200.0, &data, &mut view);
        assert!(wide.starts_with(&narrow), "{wide}");
        assert!(wide.contains(" coastlines"), "{wide}");
        assert!(!narrow.contains("coastlines"), "{narrow}");
    }

    /// The archive decides what is in the column, so it must not decide how wide a chip
    /// is: a name of 500 characters, an empty one, digits and one emoji all answer a
    /// bounded string.
    #[test]
    fn a_chip_label_is_bounded_whatever_the_archive_put_there() {
        assert_eq!(chip_label("SE", 12), "SE 12");
        assert_eq!(chip_label("1234", 7), "1234 7");
        assert_eq!(chip_label("🇸🇪", 2), "🇸🇪 2");
        assert_eq!(chip_label("   ", 3), "unknown place 3");
        let long = chip_label(&"x".repeat(500), 1);
        assert_eq!(long.chars().count(), CHIP_CHARS + 1 + 1 + 1, "{long}");
        // a newline in a place name is a space in a chip: one row, one control
        assert!(!chip_label("a\nb", 1).contains('\n'));
    }

    /// A story row is two doors, and they overlap: the row opens the reading pane, and the
    /// "Read the source" link on it opens the Reader. One press must land on exactly one of
    /// them — the press that opens the page must not also open the pane, and the press on the
    /// title must not also open the page.
    #[test]
    fn a_story_row_opens_the_story_or_its_source_but_never_both() {
        use crate::views::reader::harness;
        let mut article = story(
            "Ferry traffic resumes on the northern route",
            "The northern route reopened.",
            "The body.",
        );
        article.id = 11;
        article.url = "https://harbour.example/news/ferry".to_string();
        let data = Snapshot { articles: vec![article], ..window() };

        let ctx = harness::fresh();
        // Two quiet frames: the card's column measures itself before it clips.
        for _ in 0..2 {
            let mut choice = Choice::default();
            let _ = harness::drive(&ctx, Vec::new(), |ui| stories_card(ui, &data, &mut choice));
        }
        let mut choice = Choice::default();
        let (_, painted) =
            harness::drive(&ctx, Vec::new(), |ui| stories_card(ui, &data, &mut choice));
        assert!(
            painted.has("Read the source"),
            "the row does not offer the page:\n{}",
            painted.text
        );

        // The link's door: the address leaves for the shell, the pane stays shut.
        let at = painted.at("Read the source");
        let mut choice = Choice::default();
        let _ = harness::press(&ctx, at, |ui| stories_card(ui, &data, &mut choice));
        assert_eq!(
            crate::views::reader::take_source(&ctx).as_deref(),
            Some("https://harbour.example/news/ferry"),
            "a press on the row's source link did not hand the shell the row's address"
        );
        assert_eq!(
            choice.story, None,
            "a press on the source link also opened the reading pane"
        );

        // The row's own door: the story opens, and nothing was handed to the Reader.
        let title = painted.at("Ferry traffic resumes");
        let mut choice = Choice::default();
        let _ = harness::press(&ctx, title, |ui| stories_card(ui, &data, &mut choice));
        assert_eq!(
            choice.story,
            Some(11),
            "a press on the row did not open the story it names"
        );
        assert_eq!(
            crate::views::reader::take_source(&ctx),
            None,
            "a press on the row also opened the Reader"
        );
    }

    // ------------------------------------------------------------ the page as a reader meets it
    //
    // Every test below is one frame at the size `views::reader::harness` draws (900×620, the
    // smallest window the layout must fit), a press on something that was *painted*, and the
    // answer the page gave. No rectangle is re-derived by hand: what a reader can hit is what
    // the frame laid out.

    /// One story the page can draw without an archive behind it.
    fn row(id: i64, title: &str) -> Article {
        Article {
            id,
            title: title.to_string(),
            published: "2026-01-01T09:00:00Z".to_string(),
            source: "The Nordic Post".to_string(),
            ..Article::default()
        }
    }

    /// One dot on the plate.
    fn pin(place: &str, lat: f64, lon: f64, article: i64) -> Pin {
        Pin {
            place: place.to_string(),
            lat,
            lon,
            count: 1,
            article,
        }
    }

    /// The one sentence a part of the page prints when it has nothing, found by a fragment of
    /// it. Panics with the whole painted page when the part printed nothing at all, because the
    /// message is the only thing a reader of a failure has.
    fn invitation_of<'a>(
        painted: &'a crate::views::reader::harness::Painted,
        part: &str,
        needle: &str,
    ) -> &'a str {
        painted
            .labels
            .iter()
            .find(|(text, _)| text.contains(needle))
            .map(|(text, _)| text.as_str())
            .unwrap_or_else(|| {
                panic!("the {part} painted no empty state (looked for {needle:?}):\n{}", painted.text)
            })
    }

    /// **Every chip and every story row is a door**, and each opens the thing it names: a chip
    /// answers the country and the number printed on it, a row answers its story's id. Both are
    /// pressed at the centre of what was painted, so a row that stopped being a widget (or a
    /// chip whose face lies about where its control is) fails here.
    #[test]
    fn a_chip_and_a_story_row_each_open_what_they_name() {
        use crate::views::reader::harness;
        let mut data = window();
        data.countries = vec![("SE".to_string(), 12), ("DE".to_string(), 7)];
        data.articles = vec![
            row(11, "Ferry traffic resumes on the northern route"),
            row(12, "Chip export rules tighten in Asia"),
        ];
        let ctx = harness::fresh();
        let mut view = Overview::new();
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| show_choices(ui, &mut view, &data));

        // A chip: one control, keyed by its country, carrying the number it counted.
        let at = painted.at("SE 12");
        let (answers, _) = harness::press(&ctx, at, |ui| show_choices(ui, &mut view, &data));
        let chip = answers[1].clone();
        assert_eq!(
            chip.door(),
            Some(Door::country("SE", 12)),
            "a press on the middle of the `SE 12` chip answered {chip:?}"
        );
        assert_eq!(
            chip.story, None,
            "a press on a country chip also opened a story: {chip:?}"
        );

        // A row: the same kind of control, and its answer is the story's id.
        let at = painted.at("Ferry traffic resumes");
        let (answers, _) = harness::press(&ctx, at, |ui| show_choices(ui, &mut view, &data));
        let headline = answers[1].clone();
        assert_eq!(
            headline.story,
            Some(11),
            "a press on the first row's headline answered {headline:?}"
        );
        assert_eq!(
            headline.door(),
            None,
            "a press on a story row also opened a country: {headline:?}"
        );

        // The second row is its own door, not the first one's.
        let at = painted.at("Chip export rules");
        let (answers, _) = harness::press(&ctx, at, |ui| show_choices(ui, &mut view, &data));
        assert_eq!(
            answers[1].story,
            Some(12),
            "the second row opened {:?}",
            answers[1].story
        );
    }

    /// **An empty archive asks the reader to do something.** All three parts of the page print
    /// an invitation, each names an action the window actually has (`Refresh`, the window chips
    /// in the rail), and a page with stories in it prints none of them — an empty state is not
    /// page furniture.
    #[test]
    fn an_empty_archive_asks_the_reader_to_do_something() {
        use crate::views::reader::harness;
        let ctx = harness::fresh();
        let mut view = Overview::new();
        let empty = window();
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| show_choices(ui, &mut view, &empty));
        for (part, needle) in [
            ("map", "carries a coordinate"),
            ("country rollup", "No country has a story in this window yet"),
            ("stories", "No stories in this window yet"),
        ] {
            let sentence = invitation_of(&painted, part, needle);
            assert!(
                sentence.contains("widen the window in the rail") || sentence.contains("Refresh"),
                "the {part}'s empty state does not say what to do next: {sentence:?}"
            );
        }

        // And with something on the page, none of the three is printed.
        let mut full = window();
        full.pins = vec![pin("Stockholm", 59.3, 18.1, 11)];
        full.countries = vec![("SE".to_string(), 1)];
        full.articles = vec![row(11, "Ferry traffic resumes on the northern route")];
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| show_choices(ui, &mut view, &full));
        for needle in [
            "carries a coordinate",
            "No country has a story",
            "No stories in this window yet",
        ] {
            assert!(
                !painted.has(needle),
                "a page with a story and a chip still prints {needle:?}:\n{}",
                painted.text
            );
        }
    }

    /// **A country that counts nothing is not offered.** A chip is a promise — it prints a
    /// number and opens the list of exactly that many rows — so a rollup row of `0` is not
    /// drawn at all: there is no control where the chip would have been, and pressing that
    /// spot opens nothing.
    #[test]
    fn a_country_that_counts_nothing_is_not_offered() {
        use crate::views::reader::harness;
        let mut data = window();
        data.countries = vec![
            ("SE".to_string(), 12),
            ("NO".to_string(), 0),
            ("DE".to_string(), 7),
        ];
        let ctx = harness::fresh();
        let mut view = Overview::new();
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| show_choices(ui, &mut view, &data));

        assert!(
            painted.has("SE 12") && painted.has("DE 7"),
            "the two countries that count stories lost their chips:\n{}",
            painted.text
        );
        assert!(
            !painted.has("NO 0"),
            "the rollup printed a chip for a country with no stories:\n{}",
            painted.text
        );
        // Not merely unpainted: the page registered no widget there, so a press at that spot
        // cannot answer a door that claims nothing.
        assert!(
            ctx.read_response(place_id("NO")).is_none(),
            "a country that counts nothing still has a control that answers a press"
        );
        assert!(
            ctx.read_response(place_id("SE")).is_some(),
            "the chip that does count stories is not a control"
        );
    }

    /// A `data/atlas.db` of our own: three Swedish stories, one United States story filed in
    /// Washington, one further place (`DEU`) named by a story filed elsewhere, one duplicate
    /// and one irrelevant row. Removed when the test ends; the live archive is never opened.
    struct Fixture {
        dir: std::path::PathBuf,
        file: std::path::PathBuf,
    }

    impl Fixture {
        fn build() -> Fixture {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::time::{SystemTime, UNIX_EPOCH};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|spent| spent.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "atlas-overview-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("a temp directory");
            let file = dir.join("atlas.db");
            let conn = rusqlite::Connection::open(&file).expect("a fresh database");
            // `published` is the archive's own `YYYY-MM-DDTHH:MM:SSZ`, and SQLite writes it, so
            // the window is measured from the clock this test runs on and never drifts.
            conn.execute_batch(
                "CREATE TABLE articles(
                     id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                     published TEXT, summary TEXT, content TEXT, iso TEXT, place TEXT,
                     url TEXT, full_text INTEGER, relevant INTEGER, dupe INTEGER,
                     lat REAL, lon REAL);
                 CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
                 CREATE TABLE article_countries(article_id INTEGER, iso TEXT, is_primary INTEGER);
                 INSERT INTO sources(id, name) VALUES(1, 'The Nordic Post');
                 INSERT INTO articles(id, title, publisher, source_id, published, summary, content,
                     iso, place, url, full_text, relevant, dupe, lat, lon) VALUES
                   (1, 'Harbour crane strike ends after nine days', '', 1,
                    strftime('%Y-%m-%dT%H:%M:%SZ','now','-2 hours'), '', '', 'SE', 'Stockholm',
                    '', 1, 1, 0, 59.3, 18.1),
                   (2, 'Alpine rail freight reaches a record', '', 1,
                    strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours'), '', '', 'SE', 'Stockholm',
                    '', 1, 1, 0, 59.3, 18.1),
                   (3, 'Chip export rules tighten in Asia', '', 1,
                    strftime('%Y-%m-%dT%H:%M:%SZ','now','-4 hours'), '', '', 'SE', 'Gothenburg',
                    '', 1, 1, 0, 57.7, 12.0),
                   (4, 'A second harbour approves the contract', '', 1,
                    strftime('%Y-%m-%dT%H:%M:%SZ','now','-5 hours'), '', '', 'USA', 'Washington',
                    '', 1, 1, 0, 38.9, -77.0),
                   (5, 'Ferry traffic resumes on the northern route', '', 1,
                    strftime('%Y-%m-%dT%H:%M:%SZ','now','-6 hours'), '', '', 'NOR', 'Oslo',
                    '', 1, 1, 1, 59.9, 10.7),
                   (6, 'Drought reshapes the southern harvest', '', 1,
                    strftime('%Y-%m-%dT%H:%M:%SZ','now','-7 hours'), '', '', 'ESP', 'Madrid',
                    '', 1, 0, 0, 40.4, -3.7);
                 INSERT INTO article_countries(article_id, iso, is_primary) VALUES(1, 'DEU', 0);",
            )
            .expect("the schema and rows data.rs reads");
            drop(conn);
            Fixture { dir, file }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// **The number on a chip is the length of the list it opens**, against a real archive read
    /// through the product's own statements — and through the door the *painted* chip answers,
    /// not one built by hand.
    ///
    /// The reason is the one `app::stories_for` gives: a chip's number is a row of
    /// `Archive::countries`, which groups by `iso`, so the list has to be the country read. The
    /// fixture holds the case that used to break it — a story filed under `USA` with the place
    /// `Washington`, and a story filed under `SE` that also names `DEU` — where a *place* read of
    /// the same word holds nothing at all.
    #[test]
    fn the_number_on_a_chip_is_the_length_of_the_list_it_opens() {
        use crate::views::reader::harness;
        let fixture = Fixture::build();
        let archive = crate::data::Archive::open(&fixture.file)
            .unwrap_or_else(|why| panic!("the fixture does not open: {why}"));
        let hours = 72;
        let countries = archive.countries(hours, crate::app::COUNTRY_ROWS);
        assert_eq!(
            countries,
            vec![
                ("SE".to_string(), 3),
                ("DEU".to_string(), 1),
                ("USA".to_string(), 1)
            ],
            "the fixture's rollup is not the three rows this test was written for: the duplicate \
             and the irrelevant story must be in no country, and `DEU` is a place `SE`'s story \
             names"
        );
        let data = Snapshot {
            countries: countries.clone(),
            hours,
            ..window()
        };
        let ctx = harness::fresh();
        let mut view = Overview::new();
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| show_choices(ui, &mut view, &data));

        for (iso, count) in &countries {
            let label = chip_label(iso, *count);
            let at = painted.at(&label);
            let (answers, _) = harness::press(&ctx, at, |ui| show_choices(ui, &mut view, &data));
            let door = answers[1]
                .door()
                .unwrap_or_else(|| panic!("the {label:?} chip answered no door"));
            assert_eq!(
                door.kind,
                Kind::Country,
                "the {label:?} chip names the read that counted it"
            );
            assert_eq!(door.count, *count, "the {label:?} chip's door lost its number");
            let list = crate::app::stories_for(&archive, &door, hours, crate::app::LIST_ROWS);
            assert_eq!(
                list.len() as i64,
                (*count).min(crate::app::LIST_ROWS),
                "the {label:?} chip says {count} stories and opens {}",
                list.len()
            );
        }

        // The two words whose *place* read is empty, which is why the door carries its kind:
        // `USA` is a country no story names as a place, and `DEU` is a place no story is filed
        // in — a chip that opened either one would print a number over an empty list.
        for word in ["USA", "DEU"] {
            assert!(
                archive.place_stories(word, hours, crate::app::LIST_ROWS).is_empty(),
                "the fixture no longer holds the case this test exists for: {word} is a place"
            );
            assert!(
                !archive
                    .country_stories(word, hours, crate::app::LIST_ROWS)
                    .is_empty(),
                "{word} is a country the rollup counts and its list is empty"
            );
        }
    }

    /// **Nothing is painted outside the box the page is given**, at every audited window and
    /// with the briefing on the page — the defect that put the stories card 133 px past the
    /// content box at 1280×800 (`tests/layout_screens.rs` measured the page painting to y 761 in
    /// a box that ends at 745).
    ///
    /// Both faces are measured: the shell's page (the window's briefing, which `app.rs` always
    /// asks for) and the bare page, because they reserve different heights and a plan that only
    /// fits one of them is half a plan. The boxes are the audit's three plus the narrow case,
    /// where the map is limited by its width instead of by the room and every block under it has
    /// to fit a narrower column.
    #[test]
    fn nothing_is_painted_outside_the_box_at_any_audited_window() {
        use egui::epaint::{ClippedShape, Shape};
        // The **panel** each window leaves the page (the rail and the status row are already
        // spent by the time a page draws), and the page margins the shell draws inside it
        // (`app.rs`: 20 px each side, 16 px top and bottom). 1 092×761 is the panel the reported
        // overflow was measured in: the page's content box ends at y 745 there, and the page
        // painted to 761 because the panel — not the content box — is what clips it.
        let panels = [
            (Vec2::new(1_092.0, 761.0), "1280×800"),  // rail open
            (Vec2::new(836.0, 581.0), "900×620"),     // rail folded
            (Vec2::new(1_492.0, 1_161.0), "1680×1200"),
            (Vec2::new(656.0, 581.0), "720×620"),
        ];
        const PAGE_MARGIN: (i8, i8) = (20, 16);

        /// Why a shape may not be cut down to its clip before it is judged: the page runs under
        /// the *panel's* clip, which is taller than the content box, so a card that stands past
        /// the box is painted there — and clipped only by the window's edge.
        fn escapes(shape: &Shape, clip: egui::Rect, page: egui::Rect, out: &mut Vec<String>) {
            let rect = shape.visual_bounding_rect();
            if !rect.is_finite() {
                return;
            }
            let cut = rect.intersect(clip);
            if cut.width() <= 0.0 || cut.height() <= 0.0 {
                return;
            }
            // Half a pixel of slack: a hairline is drawn at .5 and a scissor may round out.
            if !page.expand(0.5).contains_rect(cut) {
                let what = match shape {
                    Shape::Rect(_) => "a rect".to_string(),
                    Shape::Text(text) => format!("text {:?}", head(text.galley.text(), 32)),
                    Shape::Circle(_) => "a circle".to_string(),
                    Shape::LineSegment { .. } => "a line".to_string(),
                    Shape::Path(_) => "a path".to_string(),
                    Shape::Mesh(_) => "a mesh".to_string(),
                    Shape::Vec(_) => "a group".to_string(),
                    _ => "a shape".to_string(),
                };
                out.push(format!(
                    "{what} at ({:.1}, {:.1})-({:.1}, {:.1})",
                    cut.left(),
                    cut.top(),
                    cut.right(),
                    cut.bottom()
                ));
            }
            if let Shape::Vec(items) = shape {
                for item in items {
                    escapes(item, clip, page, out);
                }
            }
        }

        let mut data = window();
        data.countries = vec![
            ("SE".to_string(), 12),
            ("DEU".to_string(), 7),
            ("a country name that is far longer than any chip".to_string(), 3),
        ];
        data.pins = vec![
            pin("Stockholm", 59.3, 18.1, 1),
            pin("Washington", 38.9, -77.0, 2),
            pin("Tokyo", 35.7, 139.7, 3),
            pin("Oslo", 59.9, 10.7, 4),
        ];
        data.articles = (0..24)
            .map(|index| row(index + 1, &format!("Story {index} of the window, one line of it")))
            .collect();
        data.articles[0].title = "A headline long enough to run past the edge of any column, \
                                   which the row must elide rather than widen the page with"
            .to_string();
        data.counts = Counts {
            relevant: 1_204,
            entities: 318,
            links: 902,
            ..Counts::default()
        };
        let ask = crate::views::brief::Ask::new("window", "", "the window", 72, None, 1);

        for (panel, window) in panels {
            let screen = egui::Rect::from_min_size(Pos2::ZERO, panel);
            // What the page is given, and the box its paint has to stay inside.
            let box_ = screen.shrink2(Vec2::new(
                f32::from(PAGE_MARGIN.0),
                f32::from(PAGE_MARGIN.1),
            ));
            for briefed in [true, false] {
                let ctx = crate::views::reader::harness::fresh();
                let mut view = Overview::new();
                let mut out = Vec::new();
                // Three frames: the plate measures the room the card has, and a scroll area clips
                // what it holds on the frame after it was laid out.
                for index in 0..3 {
                    let input = egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(index as f64 / 60.0),
                        ..Default::default()
                    };
                    let output = ctx.run_ui(input, |ui| {
                        // The shell's geometry, without its frame: `run_ui` hands the page the
                        // panel's clip rect, and the content box is only the page's *max* rect —
                        // which is exactly why a card can be painted at y 751 in a box that ends
                        // at 745 and still be seen, clipped by the window's edge alone.
                        ui.scope_builder(egui::UiBuilder::new().max_rect(box_), |ui| {
                            if briefed {
                                let _ = show_briefed(ui, &mut view, &data, Some(&ask), None);
                            } else {
                                let _ = show_choices(ui, &mut view, &data);
                            }
                        });
                    });
                    if index == 2 {
                        for ClippedShape {
                            clip_rect, shape, ..
                        } in &output.shapes
                        {
                            escapes(shape, *clip_rect, box_, &mut out);
                        }
                    }
                    output.drop_without_applying_deltas();
                }
                assert!(
                    out.is_empty(),
                    "{} shapes leave the {window} window's page box on the {} page:\n{}",
                    out.len(),
                    if briefed { "briefed" } else { "bare" },
                    out.join("\n")
                );
            }
        }
    }
}
