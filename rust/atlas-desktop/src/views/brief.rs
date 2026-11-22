//! The briefing card: the model's own sentence about a piece of the archive, above the stories.
//!
//! The reader's question is "was ist da so?" — what is *there*, without opening thirty stories.
//! The agent answers it once, writes the answer to the `briefings` table, and this module is the
//! one place it is drawn. Four things are decided here and nowhere else:
//!
//! * **The hierarchy.** The line is the card's sentence, a step above body text; the bullets are
//!   body text, one fact per line with one "·" and no numbering; the footer is caption — when it
//!   was written, over how many stories, how long a window. One accent, a hairline, radius 2.
//! * **The two doors.** The action ("Write a briefing" / "Refresh") asks the agent for a new one
//!   through the request queue; a bullet carrying `[Article #ID]` opens that story. The card says
//!   what it cannot do — a view knows no `Atlas`, so it *answers* the press and the shell acts.
//! * **Freshness.** [`stale`] compares the briefing's watermark with the archive's newest id: an
//!   id is monotonic in arrival, a publisher's timestamp is not. A stale card says so in words
//!   ("the archive has newer stories — refresh") and never repaints silently.
//! * **The empty state.** No briefing is not an error and not a blank card: one sentence saying
//!   the agent has not written one yet, and the same action, because the reader's next move is
//!   the same either way.
//!
//! What is *not* here: any call to the model, any write to the archive, and any knowledge of the
//! page it sits on. `briefing` arrives as a value, the queue as a path, and the story a door opens
//! leaves as an id ([`Ask`], [`show`]).
//!
//! ## Putting it on a page
//!
//! Three lines in the shell, then the page's own call. The two reads belong to the snapshot (once
//! per reload); the value is built once per frame and reads nothing:
//!
//! ```text
//! let briefing = archive.briefing("window", "", hours);                                 // per reload
//! let newest = archive.newest_id();                                                     // per reload
//! let ask = brief::Ask::new("window", "", "the window", hours, briefing.as_ref(), newest);
//! ```
//!
//! Overview then draws it with `overview::show_briefed(ui, view, &snapshot, Some(&ask), queue)`.
//! Articles uses the same three lines with the scope the page is about (`brief_scope`,
//! `brief_key`, `brief_note`) and draws it with
//! `articles::show_briefed(ui, view, &articles, hours, Some(&ask), queue)`. A page with neither a
//! briefing nor a queue keeps calling the plain `show`, which is the same page with `None, None`.
//! Staleness is not a fourth line: [`Ask::new`] derives it from `newest` with [`stale`]'s one rule,
//! so a page cannot draw a briefing without its freshness.

use crate::data::Briefing;
use crate::theme;
use crate::views::topics::table;
use crate::views::{clip, nf, when};

/// The page name this card remembers its own sentence under (`table::remember_said` keys on
/// it). One card, one page: the id is the card, not a page of the window.
const PAGE: &str = "brief";
/// The card's header is the row every said-sentence belongs to — the same convention Topics and
/// Monitors use for their page's own header (`table::remember_said`'s `about`).
const HEADER: i64 = 0;

/// The queue kind that asks for a briefing.
///
/// `atlas-store`'s closed set (`requests::KINDS`) and the agent's arm both spell it the same way;
/// [`tests::the_kind_and_the_payload_are_the_ones_the_agent_reads`] reads the store's own source
/// so the three cannot drift.
pub const KIND: &str = "brief";

/// The six scopes a briefing can be written for: the whole window, a country, a place, a topic, a
/// monitor, an entity. Structural — the set the store's table is keyed by — and not a word list:
/// no scope here chooses *content*, it says which question the answer is about.
pub const SCOPES: [&str; 6] = [
    "window", "country", "place", "topic", "monitor", "entity",
];

/// The action's words: **one** label for both states, because it is one action.
///
/// A card with a briefing offers a fresh one and a card without one offers the first — the same
/// `brief` request, through the same [`show`]. What a press will *do* is the empty card's
/// sentence, not the label's job: "Refresh" over nothing is still the honest name for asking
/// the agent to write one, and two labels for one act is how a reader learns to distrust both.
const REFRESH: &str = "Refresh";

/// How much of the model's text one label may carry, in characters.
///
/// A briefing is model output: the line is a sentence and a bullet is a fact, but nothing in a
/// language model guarantees that, and a 20 000-character line rebuilt into a galley on every
/// frame is a layout defect with a run time in it. The limits are generous — an ordinary briefing
/// never reaches them — and they are a *length* rule, never a word rule: nothing the model wrote
/// decides whether it is clipped, only how long it is. [`crate::views::clip`] is the crate's one
/// answer to "how much of a string the window did not write may reach a label", so it is called
/// rather than re-implemented here.
const MAX_LINE: usize = 400;
const MAX_BULLET: usize = 240;

/// What the card draws, and what the queue row needs to ask for another one.
///
/// One value rather than six arguments: the two pages that draw the card (Overview and Articles)
/// differ only in their scope, and a page that had to remember which argument came first would
/// eventually put a country's key in a window's row. [`Ask::new`] is the **one** constructor —
/// the only place these fields are written, and the only way a page can build one (the two
/// page-shaped spellings that used to sit here, `window` and `at`, were deleted the day the shell
/// moved to `Ask::new`: `grep -n "Ask::" src/app.rs` shows both call sites).
///
/// Building one is the cheapest thing a page does with a briefing: no archive is opened, no row is
/// read, no string is allocated, and every field is either borrowed from the page or a number it
/// already has (measured by [`tests::the_one_constructor_reads_nothing_and_derives_freshness`]:
/// [`Ask`] is `Copy` and a few words wide). The reads that produce `briefing` and `newest` belong
/// to the caller's snapshot — once per reload, never once per frame.
#[derive(Clone, Copy)]
pub struct Ask<'a> {
    /// The briefing that was read for this scope, if the agent has written one.
    pub briefing: Option<&'a Briefing>,
    /// One of [`SCOPES`].
    pub scope: &'a str,
    /// What the scope names: the window is empty, a country is its alpha-3, a place its own
    /// name, a topic or a monitor its id as text.
    pub key: &'a str,
    /// The window the briefing covers, as the page's window is: `hours`.
    pub hours: i64,
    /// The words the card shows for the scope ("the window", "Germany", "from USA"). Written by
    /// the page, because only the page knows what the reader picked.
    pub note: &'a str,
    /// The archive has stories the model has not seen. **Derived** by [`Ask::new`] from `newest`
    /// with [`stale`]'s rule; a page cannot set it to something the archive does not say.
    pub stale: bool,
}

impl<'a> Ask<'a> {
    /// The one constructor: the scope, the window, the briefing if one was read, and the archive's
    /// newest story id as the caller's snapshot already has it.
    ///
    /// `newest` is [`crate::data::Archive::newest_id`]. Freshness is [`stale`]'s rule and is
    /// computed here, for the reason the rule lives in one function at all: two pages must not
    /// mean two things by "newer", and a page must not be able to draw a briefing while forgetting
    /// to ask whether it is still true. Nothing is read and nothing is allocated — the answer is
    /// one comparison of two integers the caller already holds.
    ///
    /// The card, on either page:
    ///
    /// ```text
    /// let ask = brief::Ask::new(scope, key, note, hours, briefing.as_ref(), newest);
    /// // Overview: overview::show_briefed(ui, view, &snapshot, Some(&ask), queue)
    /// // Articles: articles::show_briefed(ui, view, &articles, hours, Some(&ask), queue)
    /// ```
    pub fn new(
        scope: &'a str,
        key: &'a str,
        note: &'a str,
        hours: i64,
        briefing: Option<&'a Briefing>,
        newest: i64,
    ) -> Ask<'a> {
        Ask {
            briefing,
            scope,
            key,
            hours,
            note,
            stale: briefing.is_some_and(|briefing| stale(briefing, newest)),
        }
    }
}

/// Is the archive ahead of the briefing? `newest` is [`crate::data::Archive::newest_id`].
///
/// Strictly greater: a briefing written over the story with that id *has* seen it, so the same
/// number is fresh. This is the one rule both pages use, so "newer stories" means one thing.
pub fn stale(briefing: &Briefing, newest: i64) -> bool {
    newest > briefing.watermark
}

/// The queue payload: `{"scope","key","hours","why"}` — the contract's four fields.
///
/// One function because the payload is the *wire*: the agent reads these four names, and a page
/// that built its own object would be a second definition of what a briefing request is.
pub fn payload(scope: &str, key: &str, hours: i64, why: &str) -> String {
    serde_json::json!({ "scope": scope, "key": key, "hours": hours, "why": why }).to_string()
}

/// Draw the card and answer whether the reader asked for a new briefing.
///
/// The contract's signature, and the shape a page with nothing to open uses. A bullet that leads
/// to a story needs the door ([`card_with_door`]) — the id cannot be *returned* from here without
/// changing what `true` means, so this is the plain form.
pub fn card(
    ui: &mut egui::Ui,
    briefing: Option<&Briefing>,
    scope_note: &str,
    stale: bool,
) -> bool {
    let mut open = None;
    card_with_door(ui, briefing, scope_note, stale, &mut open)
}

/// The card, with the bullets' doors.
///
/// `open` receives the article id of the bullet the reader pressed; the caller opens it. The card
/// never reports *which* scope it drew — that is the caller's value, unchanged.
pub fn card_with_door(
    ui: &mut egui::Ui,
    briefing: Option<&Briefing>,
    scope_note: &str,
    stale: bool,
    open: &mut Option<i64>,
) -> bool {
    let mut asked = false;
    crate::views::card(ui, "Briefing", scope_note, |ui| {
        match briefing {
            None => {
                // The empty state is a sentence and the action, in the same order as the full
                // card: what is here (nothing), then what a press will do about it. The sentence
                // is where "what happens next" lives, because the label is one affordance for
                // the act in both states ([`REFRESH`]) — a reader who has never seen a briefing
                // learns from these words what pressing it will produce.
                ui.label(theme::body(
                    "Nothing written for this scope yet — refresh asks the agent to write one \
                     from its stories.",
                ));
                ui.add_space(theme::space::SM);
                asked = crate::views::topics::table::request_link(
                    ui,
                    REFRESH,
                    "Ask the agent for a fresh briefing of this scope",
                );
            }
            Some(briefing) => {
                // The sentence, one step above body: the ladder's `TITLE` size in body ink. The
                // (size, face) pair is `role::CARD_TITLE`'s, which is what the design audit
                // checks; the colour stays the body's, because this is prose and not a heading.
                // The model's sentence, reduced by [`crate::assets::label`] (no path is painted)
                // and clipped by [`crate::views::clip`] (a line is a sentence, not a page). The
                // reduction comes first, so a path at the end of a 20 000-character line is still
                // named rather than cut in half.
                ui.label(
                    theme::body(&clip(&reduced(&briefing.line), MAX_LINE))
                        .size(theme::size::TITLE),
                );
                if !briefing.bullets.is_empty() {
                    ui.add_space(theme::space::SM);
                    for bullet in &briefing.bullets {
                        // One press opens one story: a bullet is one fact from one story, and
                        // the marker that says which is the door.
                        if let Some(id) = bullet_line(ui, bullet) {
                            *open = Some(id);
                        }
                    }
                }
                ui.add_space(theme::space::SM);
                ui.label(theme::caption(&footer(briefing)));
                if stale {
                    // Staleness is said in words, not in colour and not with a badge: the fact
                    // is "the archive moved on", and the action beside it is the fix.
                    ui.label(theme::caption(
                        "the archive has newer stories — refresh",
                    ));
                }
                ui.add_space(theme::space::XS);
                // The same label as the empty card's, because it is the same act: there *is* a
                // briefing, so a press asks the agent for a fresher one. One affordance, one
                // request kind, one row — the state is the sentence's business, not the link's.
                asked = crate::views::topics::table::request_link(
                    ui,
                    REFRESH,
                    "Ask the agent for a fresh briefing of this scope",
                );
            }
        }
    });
    asked
}

/// Draw the card **and** queue the request it answers.
///
/// This is the pages' entry point: the drawing is [`card_with_door`], and a press becomes one row
/// in the request queue — the window never writes the archive. `queue` is `None` for a page that
/// cannot write (a test, or a window opened without a queue); then the action is still drawn and
/// its press is answered with a sentence rather than a silence.
///
/// Returns the story a bullet opened, if any.
pub fn show(ui: &mut egui::Ui, ask: &Ask<'_>, queue: Option<&std::path::Path>) -> Option<i64> {
    let mut open: Option<i64> = None;
    let asked = card_with_door(ui, ask.briefing, ask.note, ask.stale, &mut open);
    if asked {
        // What the sentence says the press did: the scope's words, or its key when the page had
        // none — "about " with nothing after it is a sentence that lost its subject.
        let what = if ask.note.is_empty() { ask.scope } else { ask.note };
        let said = match queue {
            Some(queue) => {
                let why = format!("the reader asked for a briefing about {what}");
                let payload = payload(ask.scope, ask.key, ask.hours, &why);
                match crate::requests::push_to(queue, KIND, &payload) {
                    Ok(_) => format!("asked the agent to write a briefing about {what}"),
                    // The queue's own sentence, verbatim: it says whether the file could be
                    // made, opened or written, and it never panics on a full disk.
                    Err(problem) => problem,
                }
            }
            None => "this window has no request queue open, so nothing was asked".to_string(),
        };
        table::remember_said(ui.ctx(), PAGE, HEADER, &said);
    }
    // The answer to a press, for the frame after it: the row is written in the frame the button
    // is pressed and read in the one after, and a sentence held in a local would be a flash.
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }
    open
}

/// The footer: when it was written, how many stories it was written over, and the window.
///
/// "written 10:25 · 12 stories in 72 h". The time is [`when`]'s relative label (a time today, a
/// day-and-month before that), so the card never prints a raw stamp.
fn footer(briefing: &Briefing) -> String {
    let stories = match briefing.articles {
        1 => "1 story".to_string(),
        count => format!("{} stories", nf(count)),
    };
    format!(
        "written {} · {} in {} h",
        when(&briefing.created),
        stories,
        briefing.hours
    )
}

/// One bullet, painted, answering the story it cites if the reader pressed it.
///
/// The text is the fact with its citation **removed**: `[Article #3418]` is the model's markup for
/// "this came from that story", not something a reader reads — the marker becomes the click, and
/// the fact stays a sentence. A bullet without a marker is plain text and not clickable, because a
/// row that looks like a door and opens nothing is the defect this project keeps finding.
///
/// The door is a `Sense::click()` label, and its two keyboard promises are different in kind —
/// which is the correction to the sentence that used to stand here ("with the theme's own focus
/// ring"), measured by [`tests::a_focused_bullet_wears_the_ring_and_opens_on_space`]:
///
/// * **Reaching and opening it is egui's.** `Sense::click()` is `CLICK | FOCUSABLE`
///   (`egui-0.37.0/src/sense.rs:60`), so the bullet is in the tab order, and egui turns Space or
///   Enter on a focused widget into a primary click by itself (`egui-0.37.0/src/context.rs:1758`,
///   "Space/enter works like a primary click"). No key arm is written here; the test presses both
///   keys on a bullet focused with Tab and expects the story.
/// * **The ring is not.** egui gives a focused `Label` an *underline and active ink*
///   (`egui-0.37.0/src/widgets/label.rs:301`) and paints no focus stroke, so the theme's ring is
///   painted here from [`theme::focus_stroke`] — the same ring a focused row or control wears, so
///   one focused thing looks like every other. [`tests::a_bare_clickable_label_paints_no_ring`]
///   pins egui's half of that claim, so this workaround cannot quietly become a second ring.
fn bullet_line(ui: &mut egui::Ui, bullet: &str) -> Option<i64> {
    let (text, door) = split_citation(bullet);
    // Reduced, then clipped: the citation is already out of `text`, so clipping cannot cut a
    // door away (a marker at the end of a 5 000-character bullet still opens its story), and a
    // bullet is one fact on one line rather than a page inside a card.
    let label = format!("· {}", clip(&reduced(&text), MAX_BULLET));
    match door {
        None => {
            ui.label(theme::body(&label));
            None
        }
        Some(id) => {
            let response = ui
                .add(egui::Label::new(theme::body(&label)).sense(egui::Sense::click()))
                .on_hover_text("open this story");
            if response.has_focus() {
                // Inside the label's own box, like `views::articles`' focused row: the ring says
                // *which* fact a key would open, and it is drawn in the theme's focus ink rather
                // than egui's label underline, which is what a selected word looks like.
                ui.painter().rect_stroke(
                    response.rect,
                    theme::RADIUS,
                    theme::focus_stroke(),
                    egui::StrokeKind::Inside,
                );
            }
            // `clicked()` is the whole story for the pointer *and* the keyboard: egui turns
            // Space/Enter on a focused widget into a primary click itself
            // (`egui-0.37.0/src/context.rs:1758`: "Space/enter works like a primary click"),
            // so there is no key arm to write here — measured by
            // [`tests::a_focused_bullet_wears_the_ring_and_opens_on_space`], which focuses the
            // bullet with Tab and opens it with both keys. What egui does *not* do for a label
            // is paint the ring, which is the block above.
            response.clicked().then_some(id)
        }
    }
}

/// Model text as a label may show it: every path reduced to the name this crate's own rule
/// answers, [`crate::assets::label`].
///
/// A briefing is model output about pages we did not write, and model output quotes: a story
/// that says "the archive was read from /Users/someone/notes.txt" hands the model a path, and a
/// card that paints it verbatim puts a reader's disk on screen (measured: `tests/reader_privacy`
/// found `/Users/example/secret-notes.txt` and `C:\Users\example\secret-notes.txt` painted in
/// this card). The window already has one answer to that question — the rule the status row and
/// `data::Archive::open` use — so this calls it rather than growing a second, weaker one.
///
/// What counts as a path is **structural**, never a word list: a token that starts at a root —
/// `/` (POSIX absolute, and `//host` for a UNC share), `~` (home-relative) or a drive letter
/// (`C:`) — and nothing else. That distinction matters: `24/7` and `and/or` contain a separator
/// and are prose, so a rule that reduced anything with a slash in it would eat the sentence.
///
/// Backslashes are turned into `/` before the question is asked, because this file runs on
/// Unix and `Path::new("C:\Users\x")` is *one* component there: normalised, the drive and the
/// separators are what `assets::label` already knows how to walk, and its answer is the file's
/// own name. Whitespace is collapsed to single spaces on the way out: a label is one line, and a
/// model's newline inside a line would forge a second one.
fn reduced(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|ch| if ch == '\\' { '/' } else { ch })
        .collect();
    flat.split_whitespace()
        .map(|token| {
            if !begins_a_path(token) {
                return token.to_string();
            }
            let name = crate::assets::label(std::path::Path::new(token));
            // Nothing to name (`/` alone, `C:/`): the token is not a path to a file, and
            // inventing a placeholder would be the window writing words the model did not.
            if name.is_empty() {
                token.to_string()
            } else {
                name
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Does this token begin at a path root? The structural test [`reduced`] applies, and the only
/// one: `/`, `~`, or a drive letter before a colon.
fn begins_a_path(token: &str) -> bool {
    let bytes = token.as_bytes();
    token.starts_with('/')
        || token.starts_with('~')
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic())
}

/// A bullet split into what the reader reads and the story it cites.
///
/// The marker is `[Article #<digits>]`, written by the model exactly as the prompt asks; it can be
/// followed by a full stop ("… closed [Article #77]."). Removing it leaves that full stop floating
/// after a space, so the space before it goes too. A marker with no number, or a number that is
/// not an id, is left alone: a bullet is text, and text the model wrote is not edited on a guess.
fn split_citation(bullet: &str) -> (String, Option<i64>) {
    const OPEN: &str = "[Article #";
    let Some(start) = bullet.find(OPEN) else {
        return (bullet.trim().to_string(), None);
    };
    let after = &bullet[start + OPEN.len()..];
    let Some(close) = after.find(']') else {
        return (bullet.trim().to_string(), None);
    };
    let digits = &after[..close];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return (bullet.trim().to_string(), None);
    }
    let Ok(id) = digits.parse::<i64>() else {
        return (bullet.trim().to_string(), None);
    };
    let mut text = String::with_capacity(bullet.len());
    text.push_str(&bullet[..start]);
    text.push_str(&bullet[start + OPEN.len() + close + 1..]);
    // "… closed [Article #77]." leaves a space before the full stop; a reader sees it.
    let text = text.trim().replace(" .", ".").replace(" ,", ",");
    (text.trim().to_string(), Some(id))
}

// --------------------------------------------------------------------------------- the tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    /// Every string one headless frame painted, in draw order: what a reader would read, and
    /// the box it was painted in — which is the point a test clicks to mean "this label".
    #[derive(Default)]
    struct Painted {
        text: Vec<(String, egui::Rect)>,
        /// The surfaces painted in `theme::PANEL`: a card, a button. Their boxes are what the
        /// padding tests measure *from*.
        panels: Vec<egui::Rect>,
        /// The focus rings painted in `theme::FOCUS` at `theme::FOCUS_WIDTH`, with the stroke
        /// that made them. A ring is how a keyboard reader sees where they are; collecting them
        /// is how a test can say "this control wears the theme's ring" instead of trusting a
        /// comment.
        rings: Vec<(egui::Rect, egui::Stroke)>,
    }

    impl Painted {
        /// Does any painted run hold `needle`? Whitespace is dropped on both sides first: a
        /// galley's text carries the line breaks it was laid out with, and a bullet that wraps
        /// is still one sentence (`· The commerce department added three…` over two lines).
        fn has(&self, needle: &str) -> bool {
            let wanted = squashed(needle);
            self.text.iter().any(|(text, _)| squashed(text).contains(&wanted))
        }

        /// The centre of the run of text that holds `needle`.
        fn at(&self, needle: &str) -> egui::Pos2 {
            self.text
                .iter()
                .find(|(text, _)| text.contains(needle))
                .map(|(_, rect)| rect.center())
                .unwrap_or_else(|| panic!("nothing painted {needle:?}; painted:\n{}", self.all()))
        }

        fn all(&self) -> String {
            self.text
                .iter()
                .map(|(text, rect)| format!("{text} @ {:?} w={:.0}", rect.min, rect.width()))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// The text of a run with every whitespace character removed.
    fn squashed(text: &str) -> String {
        text.chars().filter(|ch| !ch.is_whitespace()).collect()
    }

    /// A shape tree flattened into text and boxes: a card is painted as nested `Shape::Vec`s, and
    /// a walker that stopped at the first level would see nothing at all.
    fn collect(shape: &egui::Shape, out: &mut Painted) {
        match shape {
            egui::Shape::Vec(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            egui::Shape::Text(text) => out.text.push((
                text.galley.text().to_string(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
            )),
            egui::Shape::Rect(rect) => {
                if rect.fill == theme::PANEL {
                    out.panels.push(rect.rect);
                }
                // `painter.rect_stroke` with the theme's focus stroke arrives as a transparent
                // rect carrying that stroke; the hairline of a card is 1 px and is not a ring.
                if rect.stroke.width >= theme::FOCUS_WIDTH && rect.fill == egui::Color32::TRANSPARENT
                {
                    out.rings.push((rect.rect, rect.stroke));
                }
            }
            _ => {}
        }
    }

    /// A context with the window's own theme, so sizes and colours are the shipped ones.
    fn fresh() -> egui::Context {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx
    }

    /// One headless frame of the card, at the width a page card has.
    ///
    /// The harness every view test here uses: a real `egui::Context`, one pass over the card, and
    /// the shapes read back out of the paint list. The card is drawn **twice** — egui lays a card
    /// out on the first pass and paints its final size on the second — and the second pass is the
    /// one read, because that is the frame a reader sees.
    fn frame(
        ctx: &egui::Context,
        briefing: Option<&Briefing>,
        stale: bool,
        events: Vec<egui::Event>,
    ) -> (Painted, bool, Option<i64>) {
        // Two passes, and the events only on the first: egui lays a card out on the pass it is
        // given and paints it on the next, and a click replayed on both passes is a second
        // press on a pointer that is already down — the widget would answer the first and the
        // test would read the second. So the *answer* is read from the pass that had the events
        // and the *paint* from the pass a reader would see.
        let mut asked = false;
        let mut open = None;
        let mut answered = (false, None);
        let mut painted = Painted::default();
        for pass in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(560.0, 620.0),
                )),
                events: if pass == 0 { events.clone() } else { Vec::new() },
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    asked = card_with_door(ui, briefing, "the window", stale, &mut open);
                });
            });
            if pass == 0 {
                answered = (asked, open);
            }
            painted = Painted::default();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut painted);
            }
            // egui 0.37 asserts in debug when a `FullOutput` is dropped with texture deltas still
            // in it; a headless frame has nothing to upload them to.
            output.drop_without_applying_deltas();
        }
        (painted, answered.0, answered.1)
    }

    /// One headless frame of the pages' entry point ([`show`]): the card **and** the queue it
    /// writes to.
    ///
    /// Same two-pass harness as [`frame`], for the same reason; the difference is that this one
    /// goes through the function a page calls, so a press here is a press on the real path —
    /// link, sentence, request row and all.
    fn frame_show(
        ctx: &egui::Context,
        ask: &Ask<'_>,
        queue: Option<&std::path::Path>,
        events: Vec<egui::Event>,
    ) -> (Painted, Option<i64>) {
        let mut answered = None;
        let mut painted = Painted::default();
        for pass in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(560.0, 620.0),
                )),
                events: if pass == 0 { events.clone() } else { Vec::new() },
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let opened = show(ui, ask, queue);
                    if pass == 0 {
                        answered = opened;
                    }
                });
            });
            painted = Painted::default();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut painted);
            }
            output.drop_without_applying_deltas();
        }
        (painted, answered)
    }

    /// A pointer standing where `at` is, either going down or coming up. A button answers on the
    /// release inside its own rect, which is what a reader's finger does.
    fn pointer(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
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

    /// A fresh queue file in its own temp directory: the directory to remove, and the file.
    fn temp_queue(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "atlas-brief-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let queue = dir.join("requests.db");
        (dir, queue)
    }

    /// What the queue file holds, in id order: every row the card wrote.
    fn queue_rows(queue: &std::path::Path) -> Vec<(String, String)> {
        let Ok(connection) = rusqlite::Connection::open(queue) else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare("SELECT kind, payload FROM requests ORDER BY id")
        else {
            return Vec::new();
        };
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("a read of the queue")
            .flatten()
            .collect()
    }

    /// A briefing with two bullets, one of them citing a story.
    fn written() -> Briefing {
        Briefing {
            scope: "window".to_string(),
            key: String::new(),
            hours: 72,
            watermark: 96,
            created: "2026-11-21T10:25:12Z".to_string(),
            line: "Chip export rules tightened across the region this week.".to_string(),
            bullets: vec![
                "The commerce department added three processor families to the licence list \
                 [Article #3418]."
                    .to_string(),
                "Analysts expect the curbs to slow data-centre build-outs into next year."
                    .to_string(),
            ],
            articles: 12,
            model: "qwen2.5-7b".to_string(),
            ms: 1840,
        }
    }

    #[test]
    fn the_line_and_every_bullet_are_painted() {
        let briefing = written();
        let (painted, asked, open) = frame(&fresh(), Some(&briefing), false, Vec::new());
        assert!(
            painted.has("Chip export rules tightened"),
            "the line is not painted; painted:\n{}",
            painted.all()
        );
        // Every bullet, with one leading dot and no numbering: the fact itself is painted, the
        // citation is not (it is the door, not prose).
        for bullet in &briefing.bullets {
            let (text, _) = split_citation(bullet);
            assert!(
                painted.has(&text),
                "a bullet is missing ({text:?}); painted:\n{}",
                painted.all()
            );
        }
        assert!(
            !painted.has("[Article #"),
            "the citation is painted as text instead of being a door; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.has("· The commerce department"),
            "a bullet has no leading dot; painted:\n{}",
            painted.all()
        );
        // The footer's shape, with the time the reader is shown for *this* stamp: a briefing
        // written today reads "10:25", one written last week "21 Nov", and the test must not
        // care which — the count and the window are the promise.
        let footer = footer(&briefing);
        assert!(
            footer.starts_with("written ") && footer.ends_with("12 stories in 72 h"),
            "the footer is not the shape the page promises: {footer:?}"
        );
        assert!(
            painted.has(&footer),
            "the footer is not painted; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.has(REFRESH),
            "a card with a briefing does not offer a fresher one; painted:\n{}",
            painted.all()
        );
        assert!(!asked, "nothing was pressed, yet the card answered a press");
        assert_eq!(open, None, "nothing was pressed, yet a story was opened");
    }

    #[test]
    fn a_stale_card_says_the_archive_has_moved_on() {
        let briefing = written();
        let (painted, _, _) = frame(&fresh(), Some(&briefing), true, Vec::new());
        assert!(
            painted.has("the archive has newer stories — refresh"),
            "a stale card does not say so; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.has(REFRESH),
            "a stale card does not offer the refresh; painted:\n{}",
            painted.all()
        );
        // And the rule behind the flag: an id above the watermark is newer, the same id is not.
        assert!(stale(&briefing, 97));
        assert!(!stale(&briefing, 96));
        assert!(!stale(&briefing, 12));
    }

    #[test]
    fn a_missing_briefing_says_what_will_happen_and_the_same_refresh() {
        let (painted, asked, _) = frame(&fresh(), None, false, Vec::new());
        // The sentence is where a reader learns what a press produces: the card is empty, so
        // "refresh" alone would say nothing. (This replaced "The agent has not written a
        // briefing for this yet.", which named the gap and not the consequence, when the two
        // state-dependent labels — "Write a briefing" / "Refresh" — became one.)
        assert!(
            painted.has("Nothing written for this scope yet"),
            "the empty state does not say what is here; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.has("refresh asks the agent to write one from its stories"),
            "the empty state does not say what a press will do; painted:\n{}",
            painted.all()
        );
        // The same action as a written card, with the same words on it (requirement: one
        // affordance, two states). The link is the same `show` path and the same `brief` row.
        assert!(
            painted.has(REFRESH),
            "the empty state does not offer the same refresh; painted:\n{}",
            painted.all()
        );
        assert!(
            !painted.has("· "),
            "an empty card painted a bullet; painted:\n{}",
            painted.all()
        );
        // A footer is three facts ("written … · n stories in h"); an empty card has none.
        assert!(
            !painted.has(" stories in "),
            "an empty card painted a footer; painted:\n{}",
            painted.all()
        );
        assert!(!asked);
        // The stale flag is not the empty state's business: nothing has been written, so there is
        // nothing to be stale, and the card must not claim otherwise.
        let (stale_painted, _, _) = frame(&fresh(), None, true, Vec::new());
        assert!(!stale_painted.has("newer stories"), "an empty card claims staleness");
    }

    #[test]
    fn a_press_and_release_on_the_link_asks_for_a_briefing() {
        let ctx = fresh();
        let (empty, _, _) = frame(&ctx, None, false, Vec::new());
        let at = empty.at(REFRESH);
        // Press, then release: a button answers on the release inside its own rect, which is
        // also what a reader's finger does.
        let press = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let release = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        };
        let (_, during_press, _) = frame(
            &ctx,
            None,
            false,
            vec![egui::Event::PointerMoved(at), press],
        );
        assert!(!during_press, "the button answered a press before the release");
        let (_, asked, _) = frame(&ctx, None, false, vec![egui::Event::PointerMoved(at), release]);
        assert!(asked, "a press and release on {REFRESH:?} answered nothing");
    }

    #[test]
    fn the_one_constructor_reads_nothing_and_derives_freshness() {
        let briefing = written(); // watermark 96
        let ask = Ask::new("country", "DEU", "Germany", 24, Some(&briefing), 97);
        assert_eq!(ask.scope, "country");
        assert_eq!(ask.key, "DEU");
        assert_eq!(ask.note, "Germany");
        assert_eq!(ask.hours, 24);
        assert!(
            ask.stale,
            "a story above the watermark did not make the card stale"
        );
        // The watermark's own id is *not* newer: `stale`'s one rule, through the constructor.
        assert!(!Ask::new("window", "", "the window", 72, Some(&briefing), 96).stale);
        // Nothing written is never stale, whatever the archive says.
        assert!(!Ask::new("window", "", "the window", 72, None, 10_000).stale);

        // Cheap, measured: a borrowed value, `Copy`, and a handful of words — not a snapshot of
        // the page, and nothing that has to be rebuilt per frame. Measured on x86_64: 72 bytes
        // (three `&str` = six words, one borrowed `Briefing`, one `i64`, one flag; no padding to
        // a heap allocation, because there is none).
        fn is_copy<T: Copy>(_: &T) {}
        is_copy(&ask);
        let width = std::mem::size_of::<Ask<'_>>();
        assert!(
            width <= 80,
            "Ask costs {width} bytes; it is three borrowed strings, a borrowed briefing, an i64 \
             and a flag — measured 72 on this target"
        );

        // "No reads" is structural, so it is measured on the source: between `pub fn new(` and
        // its closing brace there is no archive, no connection, no path and no read — the only
        // thing it does with the world is compare the two integers it was handed.
        let source = include_str!("brief.rs");
        let start = source.find("    pub fn new(").expect("Ask::new is in this file");
        let end = source[start..]
            .find("\n    }\n")
            .expect("the constructor's closing brace");
        let body = &source[start..start + end];
        for forbidden in ["Archive", "Connection", "open(", "read_to", "Path", "briefing("] {
            assert!(
                !body.contains(forbidden),
                "Ask::new mentions {forbidden:?}: constructing one may not read anything"
            );
        }
    }

    #[test]
    fn a_giant_line_and_bullet_are_clipped_but_keep_their_facts() {
        // Two things a language model can produce: a line of 20 000 characters with a path in it,
        // and a bullet whose citation sits at the end of 5 000. Neither may turn the card into a
        // page, and neither may lose the fact that makes it useful.
        let tail = "ENDMARK";
        let briefing = Briefing {
            scope: "window".to_string(),
            key: String::new(),
            hours: 72,
            watermark: 96,
            created: "2026-11-21T10:25:12Z".to_string(),
            line: format!(
                "read /Users/example/secret-notes.txt then {} {tail}",
                "lorem ".repeat(4_000)
            ),
            bullets: vec![format!("{} [Article #9]", "fact ".repeat(1_250))],
            articles: 12,
            model: "qwen2.5-7b".to_string(),
            ms: 1840,
        };
        let ctx = fresh();
        let (painted, _, _) = frame(&ctx, Some(&briefing), false, Vec::new());

        // Clipped: the end of the model's line is not on screen.
        assert!(
            !painted.has(tail),
            "a 20 000-character line was painted whole; painted:\n{}",
            painted.all()
        );
        // Reduced *before* it is cut: the path is named, never shown, and never lost to the cut.
        assert!(
            painted.has("secret-notes.txt"),
            "the reduced file name was clipped away; painted:\n{}",
            painted.all()
        );
        assert!(
            !painted.has("/Users/") && !painted.has("C:\\Users"),
            "a path is painted on the card; painted:\n{}",
            painted.all()
        );
        // And bounded: the line's own run is at most MAX_LINE characters plus the ellipsis.
        let line_run = painted
            .text
            .iter()
            .find(|(text, _)| text.contains("secret-notes.txt"))
            .map(|(text, _)| text.chars().filter(|ch| *ch != '\n').count())
            .expect("the line's run");
        assert!(
            line_run <= MAX_LINE + 1,
            "the line's label is {line_run} characters, over the {MAX_LINE} the card allows"
        );
        assert!(
            painted.has("…"),
            "a clipped label does not say it was clipped; painted:\n{}",
            painted.all()
        );

        // The bullet: one line, clipped, and still a door — the citation was taken out before
        // the text was cut, so the story is not lost with the characters.
        let at = painted.at("· fact");
        let (_, _, during_press) = frame(
            &ctx,
            Some(&briefing),
            false,
            pointer(at, true),
        );
        assert_eq!(during_press, None, "a press alone opened a story");
        let (_, _, opened) = frame(&ctx, Some(&briefing), false, pointer(at, false));
        assert_eq!(
            opened,
            Some(9),
            "the clipped bullet lost the story it cites; painted:\n{}",
            painted.all()
        );
    }

    /// Requirement: a press on the refresh writes **exactly one** `brief` request — in both
    /// states, because it is one action — and a frame without a press writes none.
    #[test]
    fn one_press_writes_exactly_one_brief_request_in_both_states() {
        let briefing = written();
        for (state, briefing) in [("written", Some(&briefing)), ("empty", None)] {
            let ctx = fresh();
            let (dir, queue) = temp_queue(&format!("one-{state}"));
            let ask = Ask::new("window", "", "the window", 72, briefing, 96);

            // Where the link is, from a frame with no events.
            let (painted, opened) = frame_show(&ctx, &ask, Some(&queue), Vec::new());
            assert_eq!(opened, None, "{state}: nothing pressed, yet a story opened");
            assert!(
                queue_rows(&queue).is_empty(),
                "{state}: a frame with no press wrote a request"
            );
            let at = painted.at(REFRESH);

            // Going down is not a press: the button answers on the release.
            let _ = frame_show(&ctx, &ask, Some(&queue), pointer(at, true));
            assert!(
                queue_rows(&queue).is_empty(),
                "{state}: a press alone wrote a request"
            );

            let (after, _) = frame_show(&ctx, &ask, Some(&queue), pointer(at, false));
            let rows = queue_rows(&queue);
            assert_eq!(
                rows.len(),
                1,
                "{state}: one press wrote {} rows: {rows:?}",
                rows.len()
            );
            assert_eq!(rows[0].0, KIND, "{state}: the row is not a {KIND} request");
            let value: serde_json::Value =
                serde_json::from_str(&rows[0].1).expect("the payload is JSON");
            assert_eq!(value["scope"], "window", "{state}");
            assert_eq!(value["key"], "", "{state}");
            assert_eq!(value["hours"], 72, "{state}");
            assert!(
                value["why"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("the window"),
                "{state}: the row does not say what was asked: {value}"
            );
            // The card answers the press in words, in the frame after it.
            assert!(
                after.has("asked the agent to write a briefing about the window"),
                "{state}: the press was not answered; painted:\n{}",
                after.all()
            );

            // A frame without a press writes nothing more: the row belongs to the press, not to
            // the frame — a per-frame write is the defect this asserts against.
            let _ = frame_show(&ctx, &ask, Some(&queue), Vec::new());
            assert_eq!(
                queue_rows(&queue).len(),
                1,
                "{state}: a pressless frame wrote another request"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_bullet_that_cites_a_story_is_a_door_to_it() {
        let ctx = fresh();
        let briefing = written();
        let (painted, _, _) = frame(&ctx, Some(&briefing), false, Vec::new());
        // The row is the bullet's own words — that is what the reader aims at.
        let at = painted.at("The commerce department");
        let press = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let release = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        };
        let (_, asked, open) = frame(
            &ctx,
            Some(&briefing),
            false,
            vec![egui::Event::PointerMoved(at), press],
        );
        assert_eq!(open, None, "a press alone opened a story; the release is the press");
        assert!(!asked, "a bullet press asked for a new briefing");
        let (_, _, opened) = frame(
            &ctx,
            Some(&briefing),
            false,
            vec![egui::Event::PointerMoved(at), release],
        );
        assert_eq!(
            opened,
            Some(3418),
            "the bullet citing story 3418 did not open it; painted:\n{}",
            painted.all()
        );
    }

    #[test]
    fn the_marker_becomes_a_door_and_never_a_sentence() {
        // The exact shape the prompt asks for, and the four ways a bullet can miss it.
        assert_eq!(
            split_citation("A fact [Article #12]."),
            ("A fact.".to_string(), Some(12))
        );
        assert_eq!(
            split_citation("A fact [Article #7]"),
            ("A fact".to_string(), Some(7))
        );
        assert_eq!(
            split_citation("A fact, closed [Article #77]."),
            ("A fact, closed.".to_string(), Some(77))
        );
        assert_eq!(split_citation("No citation here."), ("No citation here.".to_string(), None));
        // Not an id, not a door — and the text is not edited on a guess.
        for odd in ["[Article #]", "[Article #x]", "[Article #12"] {
            let bullet = format!("A fact {odd}.");
            assert_eq!(split_citation(&bullet), (bullet.clone(), None), "{odd}");
        }
    }

    #[test]
    fn no_label_is_a_path_and_none_shouts() {
        // The two rules `tests/disclosure.rs` holds every view to, measured on this card's own
        // paint: nothing here is a place on disk, and no label is all capitals.
        let briefing = written();
        let (painted, _, _) = frame(&fresh(), Some(&briefing), true, Vec::new());
        for (text, _) in &painted.text {
            assert!(
                !text.trim_start().starts_with('/'),
                "a label is a path: {text:?}"
            );
            let letters: Vec<char> = text.chars().filter(|ch| ch.is_alphabetic()).collect();
            let shouting = letters.len() >= 2 && letters.iter().all(|ch| ch.is_uppercase());
            assert!(!shouting, "a label shouts: {text:?}");
        }
    }

    /// A temp directory holding an archive with a `briefings` table, as the agent would leave
    /// one. The window only reads it, so the fixture is written and dropped first.
    fn archive_with(bullets: &str, watermark: i64) -> (std::path::PathBuf, crate::data::Archive) {
        let dir = std::env::temp_dir().join(format!(
            "atlas-brief-read-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("atlas.db");
        let connection = rusqlite::Connection::open(&path).expect("a fixture archive");
        connection
            .execute_batch(
                "CREATE TABLE articles (id INTEGER PRIMARY KEY, title TEXT); \
                 CREATE TABLE briefings (scope TEXT, key TEXT, hours INTEGER, watermark INTEGER, \
                   created TEXT, line TEXT, bullets TEXT, articles INTEGER, model TEXT, ms INTEGER, \
                   PRIMARY KEY(scope, key, hours));",
            )
            .expect("the fixture's tables");
        connection
            .execute(
                "INSERT INTO articles (id, title) VALUES (96, 'a story'), (97, 'a newer story')",
                [],
            )
            .expect("two stories");
        connection
            .execute(
                "INSERT INTO briefings VALUES ('window', '', 72, ?1, '2026-11-21T10:25:12Z', \
                 'Chip export rules tightened.', ?2, 12, 'qwen2.5-7b', 1840)",
                rusqlite::params![watermark, bullets],
            )
            .expect("one briefing");
        drop(connection);
        let archive = crate::data::Archive::open(&path).expect("the fixture opens read-only");
        (dir, archive)
    }

    #[test]
    fn the_row_the_agent_wrote_is_read_with_its_bullets_and_its_mark() {
        let (dir, archive) = archive_with(
            r#"["The commerce department added three families [Article #3418].","A second fact."]"#,
            96,
        );
        let briefing = archive
            .briefing("window", "", 72)
            .expect("the row the fixture wrote");
        assert_eq!(briefing.scope, "window");
        assert_eq!(briefing.key, "");
        assert_eq!(briefing.hours, 72);
        assert_eq!(briefing.watermark, 96);
        assert_eq!(briefing.line, "Chip export rules tightened.");
        assert_eq!(briefing.bullets.len(), 2);
        assert_eq!(briefing.articles, 12);
        assert_eq!(briefing.model, "qwen2.5-7b");
        assert_eq!(briefing.ms, 1840);

        // The freshness mark: an id above the watermark is newer, the same id is not.
        assert_eq!(archive.newest_id(), 97);
        assert!(stale(&briefing, archive.newest_id()));
        assert!(!stale(&briefing, 96));

        // The same scope in another window is another row, and there is no row for a scope the
        // agent never wrote.
        assert!(archive.briefing("window", "", 24).is_none());
        assert!(archive.briefing("country", "DEU", 72).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_bullets_column_loses_the_bullets_and_not_the_sentence() {
        let (dir, archive) = archive_with("not json at all", 96);
        let briefing = archive
            .briefing("window", "", 72)
            .expect("the row is still a briefing");
        assert!(
            briefing.bullets.is_empty(),
            "a broken array produced bullets: {:?}",
            briefing.bullets
        );
        assert_eq!(
            briefing.line, "Chip export rules tightened.",
            "the line went with the bullets"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // An array of something that is not text is no bullets either.
        let (dir, archive) = archive_with(r#"[1, {"fact": 2}, " "]"#, 96);
        let briefing = archive.briefing("window", "", 72).expect("the row");
        assert!(briefing.bullets.is_empty(), "{:?}", briefing.bullets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_without_the_table_answers_nothing_rather_than_failing() {
        // An archive written before the feature: no `briefings` table at all.
        let dir = std::env::temp_dir().join(format!(
            "atlas-brief-empty-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("atlas.db");
        let connection = rusqlite::Connection::open(&path).expect("a fixture archive");
        connection
            .execute_batch("CREATE TABLE articles (id INTEGER PRIMARY KEY);")
            .expect("the old schema");
        drop(connection);
        let archive = crate::data::Archive::open(&path).expect("it opens");
        assert!(archive.briefing("window", "", 72).is_none());
        // And no stories is `0`, not a missing row: nothing can be newer than a briefing over
        // the same nothing.
        assert_eq!(archive.newest_id(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_in_the_models_text_is_reduced_and_prose_is_untouched() {
        // The rule is the crate's own reducer, called — not a second spelling of it.
        assert_eq!(
            crate::assets::label(std::path::Path::new("/Users/example/secret-notes.txt")),
            "secret-notes.txt"
        );
        assert_eq!(
            reduced("The archive was read from /Users/example/secret-notes.txt before it opened."),
            "The archive was read from secret-notes.txt before it opened."
        );
        // A Windows path on Unix is one component until the backslashes are normalised, and
        // then the drive and the separators are what the reducer already knows how to walk.
        let windows = reduced(r"handed C:\Users\example\secret-notes.txt back");
        assert_eq!(windows, "handed secret-notes.txt back");
        assert!(!windows.contains(r"\Users") && !windows.contains("C:"));

        // Prose with a separator in it is prose: the test is where a token *begins*, so `24/7`,
        // `and/or` and a URL's host are not paths and are left exactly as the model wrote them.
        for prose in [
            "24/7 coverage",
            "wages and/or hours",
            "read https://example.test/story first",
            "a 50/50 split",
        ] {
            assert_eq!(reduced(prose), prose, "{prose:?} was treated as a path");
        }
        // A home-relative token begins at a root too, and a bare root names nothing: it stays.
        assert_eq!(reduced("~/notes.txt"), "notes.txt");
        assert_eq!(reduced("the / separator"), "the / separator");
        // And a model's newline does not forge a second line inside one label.
        assert_eq!(reduced("one\ntwo"), "one two");
    }

    #[test]
    fn the_kind_and_the_payload_are_the_ones_the_agent_reads() {
        // The wire, exactly: four fields, the contract's names.
        let payload = payload("country", "DEU", 72, "the reader asked");
        let value: serde_json::Value = serde_json::from_str(&payload).expect("JSON");
        assert_eq!(value["scope"], "country");
        assert_eq!(value["key"], "DEU");
        assert_eq!(value["hours"], 72);
        assert_eq!(value["why"], "the reader asked");
        assert_eq!(
            value.as_object().map(|object| object.len()),
            Some(4),
            "the payload carries more than the contract's four fields: {payload}"
        );
        // The six scopes are the store's key words: lowercase, one word each, no duplicates.
        let mut seen: Vec<&str> = SCOPES.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), SCOPES.len(), "a scope is listed twice");
        for scope in SCOPES {
            assert!(
                !scope.is_empty()
                    && scope
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch == '-'),
                "a scope is not the store's spelling: {scope:?}"
            );
        }
        // The kind is registered in `crate::requests::KINDS`, which `crate::requests`'s own test
        // holds against the store's list word for word.
        assert!(
            crate::requests::KINDS.contains(&KIND),
            "the queue does not take {KIND:?}, so this card's action cannot be answered"
        );

        // And the six scopes are the store's, read from its own source. This crate links no
        // workspace crate, so the set is a copy — and a copy that is not compared is a set that
        // drifts the day one side gains a scope (the same rule `crate::requests` applies to the
        // queue's schema and kinds).
        let store = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("atlas-store")
            .join("src")
            .join("briefings.rs");
        let Ok(source) = std::fs::read_to_string(&store) else {
            eprintln!("skipped: {} is not in this checkout", store.display());
            return;
        };
        assert!(
            source.contains(&format!("{SCOPES:?}")),
            "the store's scopes are not this card's: {SCOPES:?}"
        );
    }

    /// The action writes exactly one row, with the contract's four fields.
    ///
    /// Strong on purpose: `crate::requests::KINDS` takes `brief` (the store's closed set has it
    /// too, and `crate::requests`'s own test holds the two lists together), so a refusal here is
    /// a defect and not an expected state — the sentence would name the bug, but the assertion
    /// is the row.
    #[test]
    fn the_action_writes_one_row_with_the_contract_payload() {
        let dir = std::env::temp_dir().join(format!(
            "atlas-brief-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let queue = dir.join("requests.db");

        let briefing = written();
        let ask = Ask::new("window", "", "the window", 72, Some(&briefing), 96);
        let payload = payload(ask.scope, ask.key, ask.hours, "the reader asked");
        crate::requests::push_to(&queue, KIND, &payload)
            .unwrap_or_else(|problem| panic!("the queue refused a briefing request: {problem}"));

        let connection = rusqlite::Connection::open(&queue).expect("the queue it wrote");
        let mut statement = connection
            .prepare("SELECT kind, payload FROM requests ORDER BY id")
            .expect("the queue's own table");
        let rows: Vec<(String, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("a read of the queue")
            .flatten()
            .collect();
        assert_eq!(rows.len(), 1, "one press wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, KIND);
        assert_eq!(rows[0].1, payload, "the row is not the payload the card built");
        let value: serde_json::Value = serde_json::from_str(&rows[0].1).expect("JSON");
        assert_eq!(value["scope"], "window");
        assert_eq!(value["key"], "");
        assert_eq!(value["hours"], 72);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One frame with a lone bullet, and the story it opened: the harness for the door's own
    /// rules (the ring, the keys), which are about the widget and not about the card.
    fn bullet_frame(
        ctx: &egui::Context,
        bullet: &str,
        events: Vec<egui::Event>,
    ) -> (Painted, Option<i64>) {
        let mut answered = None;
        let mut painted = Painted::default();
        for pass in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 120.0),
                )),
                events: if pass == 0 { events.clone() } else { Vec::new() },
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let opened = bullet_line(ui, bullet);
                    if pass == 0 {
                        answered = opened;
                    }
                });
            });
            painted = Painted::default();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut painted);
            }
            output.drop_without_applying_deltas();
        }
        (painted, answered)
    }

    /// One key press, as the window's own input layer delivers it.
    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// The card's left padding, measured the way `tests/design_audit` measures it: from the
    /// surface's own left edge to the column of text inside it.
    ///
    /// This is the pin for the inset: `views::card` subtracts the hairline from `space::MD`
    /// because egui counts the stroke as part of the frame's padding, so the padding a reader
    /// sees is exactly one spacing unit value and not that value plus a pixel. Measured before
    /// that subtraction: 13.0 px here (12 + the stroke), which `design_audit` read as 14 px and
    /// rejected as off the 4 px scale.
    #[test]
    fn the_cards_padding_is_on_the_spacing_scale() {
        let briefing = written();
        let (painted, _, _) = frame(&fresh(), Some(&briefing), false, Vec::new());
        let title = painted
            .text
            .iter()
            .find(|(text, _)| text.starts_with("Briefing"))
            .map(|(_, rect)| *rect)
            .expect("the card's title is painted");
        let card = painted
            .panels
            .iter()
            .copied()
            .filter(|rect| rect.contains(title.center()) && rect.width() > 100.0)
            .max_by(|a, b| {
                a.width()
                    .partial_cmp(&b.width())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("the panel the card is painted on");
        let padding = title.left() - card.left();
        assert!(
            (padding - theme::space::MD).abs() <= 0.5,
            "the card's left padding is {padding} px, not the {} px the spacing scale says \
             (the stroke is part of an egui frame's padding)",
            theme::space::MD
        );
    }

    /// Why [`bullet_line`] paints a ring by hand: egui does not.
    ///
    /// A `Label` with the same sense, added and focused the same way, paints an *underline* and
    /// active ink (`egui-0.37.0/src/widgets/label.rs:301`) and no focus stroke. If this ever
    /// starts failing, egui has grown a ring and the workaround can go.
    #[test]
    fn a_bare_clickable_label_paints_no_ring() {
        let ctx = fresh();
        let mut painted = Painted::default();
        for pass in 0..2 {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 120.0),
                    )),
                    events: if pass == 0 { vec![press(egui::Key::Tab)] } else { Vec::new() },
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.add(
                            egui::Label::new(theme::body("· a plain clickable label"))
                                .sense(egui::Sense::click()),
                        );
                    });
                },
            );
            painted = Painted::default();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut painted);
            }
            output.drop_without_applying_deltas();
        }
        assert!(
            painted.rings.is_empty(),
            "egui painted a focus ring of its own: the hand-painted ring in `bullet_line` is now \
             a second one"
        );
    }

    /// The door's two keyboard promises, both measured on the paint list and the key path:
    /// Tab reaches the bullet (so it is in the tab order), the *theme's* ring says so, and
    /// Space or Enter opens the story without a pointer anywhere near it.
    #[test]
    fn a_focused_bullet_wears_the_ring_and_opens_on_space() {
        let ctx = fresh();
        let bullet = "A fact with a door [Article #42].";

        // Nothing focused: no ring, and no press.
        let (calm, opened) = bullet_frame(&ctx, bullet, Vec::new());
        assert!(calm.rings.is_empty(), "an unfocused bullet wears a ring");
        assert_eq!(opened, None);

        // Tab puts the keyboard on it, and the theme's ring follows.
        let (focused, opened) = bullet_frame(&ctx, bullet, vec![press(egui::Key::Tab)]);
        assert_eq!(opened, None, "moving focus opened a story");
        assert_eq!(
            focused.rings.len(),
            1,
            "a focused bullet wears no ring; painted:\n{}",
            focused.all()
        );
        let (box_, stroke) = focused.rings[0];
        assert_eq!(
            stroke,
            theme::focus_stroke(),
            "the ring is not the theme's focus stroke"
        );
        assert!(
            box_.contains(focused.at("· A fact")),
            "the ring is not around the bullet it belongs to: {box_:?}"
        );

        // Space opens it — the promise the old comment made and the code did not keep.
        let (_, opened) = bullet_frame(&ctx, bullet, vec![press(egui::Key::Space)]);
        assert_eq!(opened, Some(42), "Space on a focused bullet opened nothing");

        // Enter does the same, as on every other focused control in this window.
        let (_, opened) = bullet_frame(&ctx, bullet, vec![press(egui::Key::Enter)]);
        assert_eq!(opened, Some(42), "Enter on a focused bullet opened nothing");
    }
}
