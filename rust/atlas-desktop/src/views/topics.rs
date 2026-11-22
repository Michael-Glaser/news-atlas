//! Topics: the terms the model counts as news, and how often they hit.
//!
//! One table, not a stack of cards. Four columns, a header row that names them and
//! stays put while the rows scroll under it, one row per topic, and — for the row that
//! is open — the terms behind it and the stories that matched them. A card per topic
//! repeated the four facts that are the same for every topic and made the one fact that
//! differs (which terms) the quietest thing on the page.
//!
//! Above the table, the one thing a reader can *do* here: type what should count as news.
//! The window cannot write a topic — `data::Archive` opens the file `SQLITE_OPEN_READ_ONLY`
//! — so both actions on this page queue a request for the agent (`research`, and `topic_add`
//! for the typed words) and say what they did in one line. That is why the state column is
//! the archive's word and not a switch: nothing on this page changes a topic, it only asks.
//!
//! What the columns mean, from `docs/DATA-MODEL.md` §8: `hits` counts the stories the
//! topic was assigned to, `last_hit` is the last assignment, and a story joins a topic
//! when enough of its terms match and none of its exclude terms do.
//!
//! [`table`] is the shared half of this page and of Monitors: the column metrics, the
//! header, the row that takes the keyboard, and the open-row detail. It lives in this
//! file because `views/mod.rs` — where a module of its own would be declared — is not
//! this round's to edit; it is `pub` and used by `views::monitors`, so moving it out is
//! a two-line change the day that file is free.

use super::{heading, nf, when};
use crate::data::{Article, Topic};
use crate::theme;

/// One sentence: what a topic is in this app. Under 80 characters, so it stays one
/// line at the window's default width.
const WHAT: &str =
    "A topic is what counts as news: the terms the model matches every story against.";

/// Under an open topic the archive has never assigned a story to.
const NOTHING_YET: &str = "Nothing has matched this topic yet.";

/// Under an open topic whose stories all sit outside the window the rail is set to.
///
/// The sentence says which of the two it is, because "no stories" and "no stories *here*"
/// call for different actions: widen the window, or let a research pass run.
const NONE_IN_WINDOW: &str = "This topic has no story inside the window.";

/// The affordance under an open topic's stories.
const OPEN_IN_ARTICLES: &str = "Show these in Articles";

/// The action under an open topic: ask the supervised agent to look into it.
///
/// This window cannot collect anything — `data::Archive` is `SQLITE_OPEN_READ_ONLY` and no
/// pass can run inside the window — so the button does not research anything itself. It
/// queues one `research` request (`src/requests.rs`, `data/requests.db`) and says what it
/// did in one line. That is the whole feature: an intent, not an action.
const RESEARCH_NOW: &str = "Research this now";
const RESEARCH_HOVER: &str =
    "Ask the agent to look into this topic, beyond what the archive already has";

/// How many steps the agent may spend on a request a reader made with one click: enough to
/// follow a thread, few enough that a button press is not an open-ended job.
const RESEARCH_STEPS: u32 = 6;

/// The page's own action: the words a reader types become a topic.
///
/// The window cannot write a topic itself — `data::Archive` is `SQLITE_OPEN_READ_ONLY` and a
/// topic is compiled by the agent's pass — so the button queues one `topic_add` request
/// (`src/requests.rs`, `data/requests.db`) and says what it did in one line. One field, both
/// keys: the words are the topic's name *and* the question it asks, because inventing a
/// second name would be the window writing words nobody said.
const ADD_LABEL: &str = "Add a topic";
const ADD_HINT: &str = "what should count as news?";
const ADD_ACTION: &str = "Ask the agent to add it";
const ADD_HOVER: &str = "Queue a request for a topic with exactly these words";
const ADD_EMPTY: &str = "Type what should count as news first.";

/// The longest name and the longest question a request carries. Lengths, not words: what a
/// topic is about is the model's to decide, so nothing here filters the reader's sentence —
/// it is only bounded, because a request row is read by another process.
const NAME_MAX: usize = 120;
const QUERY_MAX: usize = 600;

/// The slot of the page's own line, beside the rows: rows are numbered from one, so zero is
/// the header (`table::remember_said`).
const HEADER: i64 = 0;

/// Painted only when the archive holds no topic. Begins with "No topic yet" and ends
/// with a full stop: the smoke test reads a page's empty state out of the page itself,
/// so this string is a contract, not a caption (`tests/smoke.rs`).
pub const EMPTY: &str = "No topic yet. A topic is what counts as news here: the terms the model \
matches every story against, and the gate on what the archive collects. This window only reads — \
run a research pass for a topic and it appears here.";

/// Above the table while no row is open: the affordance, and what the count column
/// counts. Words rather than a legend, because there is only one thing to explain.
///
/// It sits above the table and not under it because the rows now take the whole box the
/// page leaves them (see [`table::rows_height`]): a line under the list would have to come
/// out of the list's height, and the list is the thing that should fill the window.
const HINT: &str = "Select a row to read its terms. Hits are the stories that matched them.";

/// The same line while a row is open: the half that is still true, at the same height, so
/// opening a row does not move the table down.
const HINT_OPEN: &str = "Hits are the stories that matched these terms.";

/// The word for a stamp the archive never wrote.
const NEVER: &str = "never";

const WATCHING: &str = "watching";
const PAUSED: &str = "paused";

/// The page's own id: the keyboard's place in the table, the scroll offset, and the row
/// ids all hang off it, so two pages cannot share a selection or a scroll position.
const PAGE: &str = "topics";

/// The story row's two right-hand columns: the outlet, then the day. The day's width is
/// the table's own time column, so the eye reads one column of dates down the page.
const STORY_GAP: f32 = theme::space::MD;
const STORY_SOURCE_W: f32 = theme::space::COLUMN;
const STORY_WHEN_W: f32 = theme::space::COLUMN + theme::space::SM;

/// The stories the shell read for one topic, and which topic they belong to.
//////
/// The page does not query. `src/app.rs` owns the archive and its one-measured-read
/// rhythm, so it reads the stories of the topic it knows will be open and hands them here
/// together with that topic's id. The id is part of the value because the two can
/// disagree for one frame: the reader opens another row while the rows in hand are still
/// the previous row's, and drawing those under the new row would attribute one topic's
/// stories to another. A mismatch is drawn as *nothing read* — never as data.
///
/// `Copy` because the page walks the topic rows in a loop and each open row draws the
/// same list: a value this cheap (an id and a slice) is passed by value, not borrowed,
/// so the loop cannot be forced to re-borrow or clone the archive's data.
#[derive(Clone, Copy)]
pub struct Stories<'a> {
    /// The topic `rows` belong to; `None` when the shell read nothing.
    pub topic: Option<i64>,
    /// That topic's stories, newest first — the order the query answered in.
    pub rows: &'a [Article],
}

impl<'a> Stories<'a> {
    /// Nothing read: no story list is drawn, and the page claims nothing about stories.
    pub fn none() -> Stories<'a> {
        Stories {
            topic: None,
            rows: &[],
        }
    }

    /// The stories of one topic, newest first.
    pub fn of(topic: i64, rows: &'a [Article]) -> Stories<'a> {
        Stories {
            topic: Some(topic),
            rows,
        }
    }

    /// The rows to draw under `topic`: its own, or `None` for "not read".
    fn under(&self, topic: i64) -> Option<&'a [Article]> {
        match self.topic {
            Some(id) if id == topic => Some(self.rows),
            _ => None,
        }
    }
}

/// The topic whose row is open, or `None`.
///
/// The page keeps the open row in the context's own memory (see [`table::open`]), so a
/// shell that wants to read the stories before the frame can ask this: it is the same
/// answer the page will draw with.
pub fn open_topic(ctx: &egui::Context) -> Option<i64> {
    table::open(ctx, PAGE)
}

/// Open, or close, a topic's row from outside the page — what a shell does when it has
/// read a topic's stories and wants the row that owns them open.
pub fn remember_open(ctx: &egui::Context, topic: Option<i64>) {
    table::remember(ctx, PAGE, topic);
}

/// The rect the rows were given in the last frame this page drew, or `None` when the page
/// drew no table (an empty archive).
///
/// The page publishes its own scroll region for the same reason [`open_topic`] publishes
/// the open row: a headless frame can then measure the layout that shipped — that the list
/// reaches the bottom of the page box, and that the header stays above it — instead of a
/// test copying the arithmetic and measuring its own copy. It is one `Rect` in the
/// context's temporary store, rewritten every frame.
pub fn rows_region(ctx: &egui::Context) -> Option<egui::Rect> {
    ctx.data(|data| data.get_temp::<egui::Rect>(region_slot()))
}

fn remember_region(ctx: &egui::Context, region: egui::Rect) {
    ctx.data_mut(|data| data.insert_temp(region_slot(), region));
}

fn region_slot() -> egui::Id {
    egui::Id::unique(("atlas-topics-rows", PAGE))
}

/// The page, with the stories of the open topic.
///
/// Returns the story a reader asked to open: a click on a story row, Enter or Space on
/// the row the keyboard is on, or [`OPEN_IN_ARTICLES`] — which asks for the **newest** of
/// them, because that is what "show these in Articles" means in this window: the list
/// opens on the story the reader would have read first. One return value for both,
/// because both are the same request: open *this story* in the reading pane.
pub fn show_with(ui: &mut egui::Ui, topics: &[Topic], stories: Stories<'_>) -> Option<i64> {
    // The shipped window queues into its one writable file; a caller that wants another
    // target (a test, so a headless frame never touches `data/`) calls [`show_with_queue`].
    show_with_queue(ui, topics, stories, &crate::requests::queue_path())
}

/// The page, with the queue a request link writes to.
///
/// The path is a parameter and not a constant read here, because this is the boundary where
/// the window's *one* write happens: `app.rs` hands the queue it opened the window with, and
/// a test hands a temporary one. Nothing else on this page writes anything.
pub fn show_with_queue(
    ui: &mut egui::Ui,
    topics: &[Topic],
    stories: Stories<'_>,
    queue: &std::path::Path,
) -> Option<i64> {
    heading(ui, "Topics", WHAT);
    add_topic(ui, queue);
    if topics.is_empty() {
        table::empty(ui, EMPTY);
        return None;
    }

    // The row that is open, read before anything is drawn: the line above the table is
    // worded for the reader's state, and the rows below it measured from what is left.
    let mut open = open_topic(ui.ctx());
    // What the row was when the frame started: a change is a request for a new read.
    let was_open = open;
    // The affordance above the table. One line either way, so opening a row does not
    // move the table it belongs to.
    table::caption(ui, if open.is_some() { HINT_OPEN } else { HINT });

    let width = table::width(ui);
    table::header(ui, width, ["name", "state", "hits", "last activity"]);

    let ids: Vec<egui::Id> = topics.iter().map(|topic| table::row_id(PAGE, topic.id)).collect();
    // The row the arrow keys move to, applied after the loop: the row that has the
    // keyboard asks for the next one, and the request has to outlive its own frame.
    let mut next = None;
    // The story this frame's click asked for, if any.
    let mut asked: Option<i64> = None;
    let height = table::rows_height(ui);
    let rows = egui::ScrollArea::vertical()
        .id_salt("topics-rows")
        .max_height(height)
        // Both axes absolute: the region takes the height the page leaves it, so the list
        // — and its scroll bar — reach the bottom of the window instead of a content-high
        // block floating in an empty page. `rows_height` is what makes the height honest.
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, topic) in topics.iter().enumerate() {
                let response = table::row(ui, width, ids[index], &line(topic), open == Some(topic.id));
                if table::activate(ui, &response) {
                    open = if open == Some(topic.id) { None } else { Some(topic.id) };
                }
                let step = table::step(ui, &response);
                if step != 0 {
                    next = Some(landed(index, step, topics.len()));
                }
                if open == Some(topic.id) {
                    terms_and_ask(ui, width, topic, queue);
                    asked = stories_under(ui, width, topic, stories).or(asked);
                }
                if response.gained_focus() {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
    // The region the rows were given, published: a headless frame can then measure that
    // the list fills the page box rather than assert it from the source.
    remember_region(ui.ctx(), rows.inner_rect);

    if let Some(index) = next {
        table::focus(ui, ids[index]);
    }
    if open.is_some() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        open = None;
    }
    // A row that opened or shut this frame changes what the shell has to read (the
    // stories of the open topic), and the shell reads at the top of a frame, before this
    // page is drawn. Without this request the stories would wait for the reader's next
    // mouse move to arrive — a click that looks dead for as long as the pointer rests,
    // which is the complaint this page is here to answer. One frame, then the rows.
    if open != was_open {
        ui.ctx().request_repaint();
    }
    remember_open(ui.ctx(), open);
    asked
}

/// The shell's entry point until the window hands the stories over.
///
/// It has no archive in its hand, so an open topic lists no stories and nothing is
/// reported — the page looks exactly as it did before the story list existed. One line to
/// replace with [`show_with`]; keeping it means the shell keeps its signature and its
/// build while the wire is connected (`src/app.rs`, the `Page::Topics` arm).
pub fn show(ui: &mut egui::Ui, topics: &[Topic]) {
    let _ = show_with(ui, topics, Stories::none());
}

/// The page's own action, above the table: the reader's words become a topic.
///
/// Drawn on every state of the page, the empty archive included: a page that says "run a
/// research pass" and offers nothing to press is the dead end this band removes. The field's
/// text lives in the context's memory rather than in a `&mut` the shell holds, for the same
/// reason the open row does — the page stays a function of its data.
fn add_topic(ui: &mut egui::Ui, queue: &std::path::Path) {
    let slot = egui::Id::unique(("atlas-topic-draft", PAGE));
    let mut draft = ui.data(|data| data.get_temp::<String>(slot)).unwrap_or_default();
    let mut submit = false;
    // Wrapped, not one row: at the narrowest page the window allows the field, its label and
    // the link are wider than the box, and a row that overflows it pushes the table (and its
    // scroll bar) past the page's own edge. The field takes what is left of the box, between
    // two columns and four, so it neither collapses nor eats the list.
    ui.horizontal_wrapped(|ui| {
        let room = ui.available_width();
        ui.label(theme::caption(ADD_LABEL));
        let field_width = (room - theme::space::COLUMN * 2.0)
            .clamp(theme::space::COLUMN * 2.0, theme::space::COLUMN * 4.0);
        let field = ui.add(
            egui::TextEdit::singleline(&mut draft)
                .hint_text(ADD_HINT)
                .desired_width(field_width),
        );
        // Enter in the field: the keyboard's way to press the link beside it.
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        submit = table::request_link(ui, ADD_ACTION, ADD_HOVER) || submit;
    });
    if submit {
        let typed = draft.trim().to_owned();
        if typed.is_empty() {
            table::remember_said(ui.ctx(), PAGE, HEADER, ADD_EMPTY);
        } else {
            let name = crate::views::clip(&typed, NAME_MAX);
            let query = crate::views::clip(&typed, QUERY_MAX);
            let payload = crate::requests::topic_add_payload(&name, &query);
            let said = match crate::requests::push_to(queue, "topic_add", &payload) {
                Ok(_) => {
                    // A sent request clears the field; a refused one keeps the words, so a
                    // failure costs a correction rather than retyping the sentence.
                    draft.clear();
                    format!("asked the agent to add a topic about {name}")
                }
                Err(problem) => refusal(&problem),
            };
            table::remember_said(ui.ctx(), PAGE, HEADER, &said);
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|data| data.insert_temp(slot, draft));
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }
    ui.add_space(theme::space::SM);
}

/// The open topic's terms, and the one thing the row can *do*.
///
/// The terms, the action and the answer live in one band because they are one subject: the
/// reader opened a row and now asks something about exactly that row. The band is drawn
/// before the stories so the action sits where the eye already is (directly under the name
/// it is about) rather than after a list of unknown length.
///
/// On a press the window queues one `research` request and then says what happened — in one
/// line, in the page's own voice: `asked the agent to look into chips`, or the sentence the
/// queue returned when it could not. **Nothing is read again**: a request is an intent for
/// another process, so the archive this page draws is exactly as stale as it was one click
/// ago, which is honest. The sentence is remembered per row (`table::remember_said`), so it
/// survives the frames between the press and the reader's next move.
fn terms_and_ask(ui: &mut egui::Ui, width: f32, topic: &Topic, queue: &std::path::Path) {
    let line_width = table::detail_width(width);
    let mut pressed = false;
    table::detail_with(ui, width, |ui| {
        ui.set_min_width(line_width);
        ui.set_max_width(line_width);
        ui.add(egui::Label::new(theme::dim(&terms(topic))).wrap());
        ui.add_space(theme::space::XS);
        pressed = table::request_link(ui, RESEARCH_NOW, RESEARCH_HOVER);
        if let Some(said) = table::said_about(ui.ctx(), PAGE, topic.id) {
            ui.add_space(theme::space::XS);
            table::said_line(ui, &said);
        }
    });
    if !pressed {
        return;
    }
    let payload = crate::requests::research_payload(&topic.name, RESEARCH_STEPS);
    let said = match crate::requests::push_to(queue, "research", &payload) {
        Ok(_) => format!("asked the agent to look into {}", subject(topic)),
        // The queue's own sentence, folded to one line: `requests.rs` writes why it could not
        // (a locked file, a path that cannot be made) and never a panic; [`refusal`] drops the
        // path and ends the line.
        Err(problem) => refusal(&problem),
    };
    table::remember_said(ui.ctx(), PAGE, topic.id, &said);
    // The sentence is drawn next frame; ask for that frame now, so the click is answered in
    // the frame the reader will look at rather than whenever the pointer next moves.
    ui.ctx().request_repaint();
}

/// A topic as a sentence can carry it: its name, bounded, and a noun when the archive stored
/// no name at all. The bound is a length, not a word list — the name is whatever the model
/// wrote.
fn subject(topic: &Topic) -> String {
    let name = crate::views::clip(topic.name.trim(), 60);
    if name.is_empty() {
        "this topic".to_string()
    } else {
        name
    }
}

/// A refused write, as one sentence a reader can act on.
///
/// `requests.rs` answers with the driver's own words — `the window could not open the request
/// queue: unable to open database file: /Users/…/requests.db` — which is three clauses and the
/// reader's home directory: the path is not something anyone can act on from this page, and a
/// filesystem path is a private detail to paint in a window. What survives is the queue's own
/// clause and the reason behind it, ended with a full stop, so a refusal is one line. The
/// underlying message keeps its path in the log, where it belongs.
fn refusal(problem: &str) -> String {
    let said = problem.trim().trim_end_matches('.');
    let mut parts = said.splitn(3, ": ");
    let headline = parts.next().unwrap_or_default().trim();
    let headline = if headline.is_empty() {
        "the window could not queue that request"
    } else {
        headline
    };
    match parts.next().map(str::trim).filter(|reason| readable(reason)) {
        Some(reason) => format!("{headline}: {}.", reason.trim_end_matches('.')),
        None => format!("{}.", headline.trim_end_matches('.')),
    }
}

/// Whether a clause of the driver's message says something a reader can act on: a path does
/// not, and neither does nothing at all.
fn readable(reason: &str) -> bool {
    !reason.is_empty() && !reason.contains('/') && !reason.contains('\\')
}

/// One sentence above a list shorter than the count on its row: which number is the list in
/// hand and which is the archive's own count of everything the topic ever matched.
fn showing(shown: usize, matched: i64) -> String {
    format!(
        "Showing the newest {} of {} matched stories.",
        nf(shown as i64),
        nf(matched)
    )
}

/// The stories of the open topic, in a band of their own under its terms.
///
/// Returns the story a reader asked to open, or `None`. Nothing is drawn when the shell
/// read nothing (`Stories::none()`, or stories for a different topic): the page cannot
/// claim a topic has no stories when it was not told, so the band is absent rather than
/// empty, and the open row keeps the look it had.
fn stories_under(
    ui: &mut egui::Ui,
    width: f32,
    topic: &Topic,
    stories: Stories<'_>,
) -> Option<i64> {
    let rows = stories.under(topic.id)?;
    let mut asked = None;
    table::detail_with(ui, width, |ui| {
        if rows.is_empty() {
            // Which of the two sentences is true is a fact the archive told us: `hits`
            // counts every story that ever matched, the rows came back inside the window.
            let sentence = if topic.hits <= 0 {
                NOTHING_YET
            } else {
                NONE_IN_WINDOW
            };
            ui.add(egui::Label::new(theme::dim(sentence)).wrap());
            return;
        }
        // The row's count is every story the topic ever matched; the list is what the shell
        // read — this window, and the shell's own cap. When the two differ the band says
        // which is which, instead of printing a count over a shorter list and letting the
        // reader find out by counting.
        if (rows.len() as i64) < topic.hits {
            ui.add(egui::Label::new(theme::dim(&showing(rows.len(), topic.hits))).wrap());
            ui.add_space(theme::space::XS);
        }
        for story in rows {
            let response = story_row(ui, story, topic.id);
            if table::activate(ui, &response) {
                asked = Some(story.id);
            }
        }
        ui.add_space(theme::space::XS);
        let button = ui
            .add(egui::Button::new(theme::caption(OPEN_IN_ARTICLES)))
            .on_hover_text("Open the newest of these stories in the reading pane");
        if button.clicked() {
            asked = asked.or_else(|| rows.first().map(|story| story.id));
        }
    });
    asked
}

/// A story row's identity: stable across frames, and unique per topic so two topics'
/// lists cannot share a focus ring.
pub fn story_row_id(topic: i64, story: i64) -> egui::Id {
    egui::Id::unique(("atlas-topic-story", topic, story))
}

/// One story of an open topic: title, outlet, day — and a click that reports its id.
///
/// The height, the hairline and the focus ring are the table's, because this row sits
/// under one and reads as part of it; only the columns are its own (there is no state and
/// no count on a story).
fn story_row(ui: &mut egui::Ui, story: &Article, topic: i64) -> egui::Response {
    let width = ui.available_width().max(theme::space::COLUMN * 2.0);
    let (_auto, rect) = ui.allocate_space(egui::vec2(width, table::ROW_H));
    let response = ui
        .interact(rect, story_row_id(topic, story.id), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if response.hovered() {
        ui.painter().rect_filled(rect, theme::RADIUS, theme::RAISED);
    }
    ui.painter()
        .hline(rect.x_range(), rect.bottom(), theme::soft_line());

    let y = rect.center().y;
    let right = rect.right();
    let stamp = egui::Rangef::new(right - STORY_WHEN_W, right);
    let source = egui::Rangef::new(
        stamp.min - STORY_GAP - STORY_SOURCE_W,
        stamp.min - STORY_GAP,
    );
    let title = egui::Rangef::new(rect.left(), (source.min - STORY_GAP).max(rect.left()));

    table::text(
        ui,
        title,
        y,
        &story_title(story),
        table::body_font(),
        theme::INK,
        egui::Align2::LEFT_CENTER,
    );
    let outlet = story.source.trim();
    if !outlet.is_empty() {
        table::text(
            ui,
            source,
            y,
            outlet,
            table::caption_font(),
            theme::DIM,
            egui::Align2::RIGHT_CENTER,
        );
    }
    if !story.published.trim().is_empty() {
        table::text(
            ui,
            stamp,
            y,
            &when(&story.published),
            table::caption_font(),
            theme::DIM,
            egui::Align2::RIGHT_CENTER,
        );
    }
    if response.has_focus() {
        ui.painter()
            .rect_stroke(rect, theme::RADIUS, theme::focus_stroke(), egui::StrokeKind::Inside);
    }
    response
}

/// A story's row name. The archive can hold a story without a title; a row still has to
/// be clickable and say what it is, and it may not borrow the empty state's opening words.
fn story_title(story: &Article) -> String {
    if story.title.trim().is_empty() {
        "Untitled story".to_string()
    } else {
        story.title.clone()
    }
}

/// One row's four facts, already worded for the columns.
fn line(topic: &Topic) -> table::Line {
    table::Line {
        name: if topic.name.trim().is_empty() {
            format!("topic {}", topic.id)
        } else {
            topic.name.clone()
        },
        state: if topic.active { WATCHING } else { PAUSED },
        on: topic.active,
        hits: nf(topic.hits),
        last: if topic.last_hit.is_empty() {
            NEVER.to_string()
        } else {
            when(&topic.last_hit)
        },
    }
}

/// What the row cannot say: the terms themselves, as the engine stores them.
fn terms(topic: &Topic) -> String {
    let keywords = topic.keywords.trim();
    if keywords.is_empty() {
        // never "No terms …": a page with data may not paint the empty state's own
        // opening words (`tests/smoke.rs`)
        "This topic has no terms stored, so nothing matches it.".to_string()
    } else {
        format!("Terms: {keywords}")
    }
}

/// Where the arrow keys land, clamped to the list: the first row is a stop, not a wrap.
fn landed(index: usize, step: i32, len: usize) -> usize {
    if step > 0 {
        (index + 1).min(len.saturating_sub(1))
    } else {
        index.saturating_sub(step.unsigned_abs() as usize)
    }
}

// ---------------------------------------------------------------------------- table

/// The table both list pages draw: four columns with a header that stays put, rows
/// that take the keyboard, and the detail an open row owes the reader.
///
/// What it does not decide: what a row is called, what the state words are, or what
/// the detail says. Those are the page's, which is why this module can serve Topics and
/// Monitors without knowing which one is drawing.
pub mod table {
    use egui::text::{LayoutJob, TextWrapping};
    use egui::{Align, Align2, Color32, CursorIcon, FontId, Id, Rangef, Response, Sense, StrokeKind};

    use crate::theme;
    use crate::theme::space;

    /// The three named columns are fixed and hug the right edge; the name takes what is
    /// left. Sizes are multiples of [`space::UNIT`] for the same reason every gap is.
    const STATE_W: f32 = space::COLUMN;
    const HITS_W: f32 = space::COLUMN - space::XL;
    const LAST_W: f32 = space::COLUMN + space::SM;
    /// Between two cells, and the row's own left and right padding.
    const CELL_GAP: f32 = space::MD;
    const PAD_X: f32 = space::SM;
    /// A row and a header line: both on the spacing scale.
    pub const ROW_H: f32 = space::XXL;
    const HEAD_H: f32 = space::XL;
    /// The gutter a scroll bar paints in, kept out of the columns so the bar can never
    /// sit on the last cell. The header is drawn to the same width, so the two align.
    const BAR_GUTTER: f32 = space::MD;
    /// The name column never gets narrower than this.
    const NAME_MIN: f32 = space::COLUMN + space::XXL + space::XS;
    /// The width a sentence may use: about 80 characters at 12 px, so a detail or an
    /// empty state is read, not scanned across a 1400 px window.
    const PROSE_W: f32 = space::COLUMN * 5.0;
    /// What sits below the rows inside the page box: one unit of air, so the last row
    /// does not touch the window's own edge.
    ///
    /// Only the page's own bottom is reserved here. The status row and the page's outer
    /// margins are already outside the box this page is given, so reserving them again
    /// (as this constant did when the height was measured from the viewport) left the
    /// list short by the height of everything between the box and the window's edge. The
    /// affordance line that used to sit under the table now sits above it, because a line
    /// under the list is a line taken out of the list.
    const BELOW: f32 = space::XS;
    /// The state dot's radius, and the room between it and its word.
    const DOT_R: f32 = 3.0;
    const DOT_GAP: f32 = space::SM;

    /// One row of the table, worded by the page that draws it.
    pub struct Line {
        pub name: String,
        /// The state in one word: "watching" or "paused".
        pub state: &'static str,
        /// Whether that state is the on one — it decides the dot, not the word.
        pub on: bool,
        /// The count, already grouped (`views::nf`).
        pub hits: String,
        /// The last activity, already shortened (`views::when`), or "never".
        pub last: String,
    }

    /// The width the table may use: the window's, less the gutter a scroll bar needs.
    pub fn width(ui: &egui::Ui) -> f32 {
        (ui.available_width() - BAR_GUTTER).max(NAME_MIN + STATE_W + HITS_W + LAST_W)
    }

    /// The height the rows may take before they scroll: from here to the bottom of the
    /// box this page was given, less what the page still has to draw under them.
    ///
    /// Measured from [`egui::Ui::max_rect`], not from `viewport_rect()`. The viewport is
    /// the whole window: it includes the rail, the status row and the page's own margins,
    /// none of which this page may paint in. A list measured against the viewport
    /// therefore claimed a height it could not use and, with a scroll area that shrank to
    /// its content, drew a short block and left the rest of the window empty — measured
    /// at 18 % of the page box at 2400×1400 and 43 % at 900×620, where the block was
    /// 112 px tall at every size because no height was ever asked of it.
    pub fn rows_height(ui: &egui::Ui) -> f32 {
        let bottom = ui.max_rect().bottom() - BELOW;
        let height = bottom - ui.cursor().top();
        // A `Ui` inside a scroll area that has no maximum reports an unbounded rect; the
        // window's own height is then the honest cap rather than an infinity handed to
        // `ScrollArea::max_height`.
        let height = if height.is_finite() {
            height
        } else {
            ui.ctx().viewport_rect().height() - BELOW
        };
        height.max(ROW_H * 3.0)
    }

    /// The four columns of one line, left to right, in absolute coordinates.
    pub fn spans(left: f32, width: f32) -> [Rangef; 4] {
        let right = left + width - PAD_X;
        let last = Rangef::new(right - LAST_W, right);
        let hits = Rangef::new(last.min - CELL_GAP - HITS_W, last.min - CELL_GAP);
        let state = Rangef::new(hits.min - CELL_GAP - STATE_W, hits.min - CELL_GAP);
        let name = Rangef::new(left + PAD_X, (state.min - CELL_GAP).max(left + PAD_X));
        [name, state, hits, last]
    }

    /// The header row: the four column names, one hairline under them.
    pub fn header(ui: &mut egui::Ui, width: f32, labels: [&str; 4]) {
        let (_auto, rect) = ui.allocate_space(egui::vec2(width, HEAD_H));
        let spans = spans(rect.left(), width);
        for (index, label) in labels.iter().enumerate() {
            // a count is read down a column, so its heading sits over the figures
            let align = if index == 2 { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
            text(ui, spans[index], rect.center().y, label, caption_font(), theme::DIM, align);
        }
        ui.painter().hline(
            Rangef::new(rect.left(), rect.left() + width),
            rect.bottom(),
            theme::line(),
        );
    }

    /// One row. Answers its own response so the page can open it, move the keyboard, or
    /// scroll it into view.
    pub fn row(ui: &mut egui::Ui, width: f32, id: Id, line: &Line, open: bool) -> Response {
        let (_auto, rect) = ui.allocate_space(egui::vec2(width, ROW_H));
        let response = ui
            .interact(rect, id, Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);

        // The fill is painted before the text, and the text is painted by the same
        // painter, so the order of the calls is the order of the pixels.
        if open {
            ui.painter().rect_filled(rect, theme::RADIUS, theme::SELECT);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, theme::RADIUS, theme::RAISED);
        }
        if !open {
            // an open row's hairline is drawn under its detail instead, so the row and
            // its detail read as one block
            ui.painter().hline(rect.x_range(), rect.bottom(), theme::soft_line());
        }

        let spans = spans(rect.left(), width);
        let y = rect.center().y;
        text(ui, spans[0], y, &line.name, body_font(), theme::INK, Align2::LEFT_CENTER);

        let dot = egui::pos2(spans[1].min + DOT_R, y);
        if line.on {
            ui.painter().circle_filled(dot, DOT_R, theme::INK);
        } else {
            ui.painter()
                .circle_stroke(dot, DOT_R, egui::Stroke::new(theme::HAIRLINE_WIDTH, theme::DIM));
        }
        let word = Rangef::new(dot.x + DOT_R + DOT_GAP, spans[1].max);
        text(
            ui,
            word,
            y,
            line.state,
            body_font(),
            if line.on { theme::INK } else { theme::DIM },
            Align2::LEFT_CENTER,
        );

        text(ui, spans[2], y, &line.hits, number_font(), theme::INK, Align2::RIGHT_CENTER);
        text(ui, spans[3], y, &line.last, caption_font(), theme::DIM, Align2::LEFT_CENTER);

        // The keyboard's place is painted last and inside the row: inside because the
        // rows are flush, so a ring outside one would be drawn over its neighbour, and
        // last so no cell can cover it.
        // A row the pointer pressed is the row the keys act on. `egui` gives focus on click
        // only to widgets that ask for it, and a row that opens but never takes the keyboard
        // leaves the arrows and Enter with nothing to move — the row would answer the mouse
        // and ignore the keys.
        if response.clicked() {
            response.request_focus();
        }
        if response.has_focus() {
            ui.painter()
                .rect_stroke(rect, theme::RADIUS, theme::focus_stroke(), StrokeKind::Inside);
        }
        response
    }

    /// The width a sentence in a detail band may use: the table's width less the band's two
    /// margins, capped at [`PROSE_W`] so a sentence under a row is read rather than scanned
    /// across a 1400 px window. One function because the bands must line up: a body that
    /// worked the arithmetic out again would be one margin away from the band above it.
    pub fn detail_width(width: f32) -> f32 {
        (width - PAD_X - space::LG).min(PROSE_W)
    }

    /// The block under an open row: what the columns cannot say, on the row's own fill
    /// so the two read as one selection.
    pub fn detail(ui: &mut egui::Ui, width: f32, prose: &str) {
        let line_width = detail_width(width);
        detail_with(ui, width, |ui| {
            ui.set_min_width(line_width);
            ui.set_max_width(line_width);
            ui.add(egui::Label::new(theme::dim(prose)).wrap());
        });
    }

    /// The same block, drawn by the page: the same fill, the same margins, the same
    /// hairline under it — and the caller's own rows inside.
    ///
    /// A second band on the same fill reads as part of the same open row, which is why
    /// an open topic's stories are drawn in one of these under its terms rather than in
    /// a card of their own: the reader opened one row, and everything under it belongs to
    /// that row. The width is the caller's because a list of story titles wants the
    /// table's width, while a line of prose wants a width a reader can follow.
    pub fn detail_with<R>(ui: &mut egui::Ui, width: f32, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let inner = (width - PAD_X - space::LG).max(space::COLUMN * 2.0);
        let frame = egui::Frame::NONE.fill(theme::SELECT).inner_margin(egui::Margin {
            left: PAD_X as i8,
            right: space::LG as i8,
            top: space::XS as i8,
            bottom: space::SM as i8,
        });
        let shown = frame.show(ui, |ui| {
            ui.set_min_width(inner);
            ui.set_max_width(inner);
            body(ui)
        });
        ui.painter().hline(
            shown.response.rect.x_range(),
            shown.response.rect.bottom(),
            theme::soft_line(),
        );
        shown.inner
    }

    /// A request link: the page's own vocabulary, the same shape as
    /// [`super::OPEN_IN_ARTICLES`] — a button whose *text* is its label, at caption size, in
    /// the theme's own role so it cannot pick a size or a colour of its own.
    ///
    /// **Framed on purpose.** `Button::frame(false)` looks like a text link and is what the
    /// chat's transcript uses, but egui draws the focus ring *as* the frame (`widgets.active`
    /// carries [`theme::focus_stroke`]), so a frameless button has no visible keyboard focus
    /// — and this button is the page's one action, so it has to be reachable and visible
    /// from the keyboard. The frame is the theme's hairline, not a new ornament.
    pub fn request_link(ui: &mut egui::Ui, label: &str, hover: &str) -> bool {
        ui.add(egui::Button::new(theme::caption(label)))
            .on_hover_text(hover)
            .clicked()
    }

    /// The one line a page says after the reader asked for something: what the window did,
    /// or why it could not.
    ///
    /// Kept in the context's own memory, beside the open row and for the same reason — the
    /// page stays a function of its data, and the sentence outlives the frame it was made in
    /// (a request is answered in the frame after the click, and a sentence that vanished on
    /// mouse-move would be a flash, not an answer). `about` is the row it concerns; **0 is
    /// the page's own header**, which is where a monitor's "add" request answers.
    pub fn remember_said(ctx: &egui::Context, page: &str, about: i64, said: &str) {
        ctx.data_mut(|data| data.insert_temp(said_slot(page, about), said.to_owned()));
    }

    /// What the page last said about `about`, if it said anything.
    pub fn said_about(ctx: &egui::Context, page: &str, about: i64) -> Option<String> {
        ctx.data(|data| data.get_temp::<String>(said_slot(page, about)))
    }

    fn said_slot(page: &str, about: i64) -> Id {
        Id::unique(("atlas-request-said", page, about))
    }

    /// The sentence itself: one line of prose, at a width a reader can follow, truncated by
    /// a length rather than by a word list — an error from the file system can be any
    /// length, and a galley rebuilt from a 20 000-character message every frame is the
    /// defect [`super::table::text`] exists to prevent, one layer up.
    pub fn said_line(ui: &mut egui::Ui, sentence: &str) {
        ui.scope(|ui| {
            ui.set_max_width(PROSE_W);
            ui.add(egui::Label::new(theme::dim(&crate::views::clip(sentence, 240))).wrap());
        });
    }

    /// A page with nothing in it: one sentence, at a width a reader can follow, and it
    /// has to say what to do next.
    pub fn empty(ui: &mut egui::Ui, sentence: &str) {        ui.scope(|ui| {
            ui.set_max_width(PROSE_W);
            ui.add(egui::Label::new(theme::dim(sentence)).wrap());
        });
    }

    /// The line under the table: the affordance, and the meaning of the count column.
    pub fn caption(ui: &mut egui::Ui, sentence: &str) {
        ui.add_space(space::SM);
        ui.scope(|ui| {
            ui.set_max_width(PROSE_W);
            ui.add(egui::Label::new(theme::caption(sentence)).wrap());
        });
    }

    /// The row a page has open. Kept in the context's own memory rather than in `app.rs`,
    /// so a page stays a function of its data and the page that owns it needs no state.
    pub fn open(ctx: &egui::Context, page: &str) -> Option<i64> {
        ctx.data(|data| data.get_temp::<Option<i64>>(slot(page)).flatten())
    }

    pub fn remember(ctx: &egui::Context, page: &str, row: Option<i64>) {
        ctx.data_mut(|data| data.insert_temp(slot(page), row));
    }

    fn slot(page: &str) -> Id {
        Id::unique(("atlas-table-open", page))
    }

    /// A row's identity: stable across frames, unique across pages.
    pub fn row_id(page: &str, row: i64) -> Id {
        Id::unique(("atlas-table-row", page, row))
    }

    /// A click, or Enter/Space on the row the keyboard is on. The keys are taken so no
    /// other widget sees the same press.
    pub fn activate(ui: &egui::Ui, response: &Response) -> bool {
        if response.clicked() {
            return true;
        }
        if !response.has_focus() {
            return false;
        }
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Space)
        })
    }

    /// Which way the arrow keys move the keyboard: `-1` up, `1` down, `0` nowhere.
    pub fn step(ui: &egui::Ui, response: &Response) -> i32 {
        if !response.has_focus() {
            return 0;
        }
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                1
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                -1
            } else {
                0
            }
        })
    }

    pub fn focus(ui: &egui::Ui, row: Id) {
        ui.memory_mut(|memory| memory.request_focus(row));
    }

    // ---------------------------------------------------------------- painting

    /// The three fonts the table draws in, each taken from the role table rather than
    /// built from a size and a face here.
    ///
    /// `number_font` is why: it was `(size::BODY, Weight::Regular)` — 13 px in the numeric
    /// face — a (size, face) pair **no role produces**, because the ladder has no
    /// monospaced body size. `tests/design_audit.rs` reads the pair off the painted galley
    /// and refuses it, correctly: a pair invented here is a step off the ladder that no
    /// theme change can reach. [`theme::role::NUMBER`] is 15 px in the numeric face, which is
    /// also what a column of figures wants.
    pub fn body_font() -> FontId {
        theme::role::BODY.font()
    }

    fn number_font() -> FontId {
        theme::role::NUMBER.font()
    }

    pub fn caption_font() -> FontId {
        theme::role::CAPTION.font()
    }

    /// Draw one single-line cell, elided with `…` when it does not fit, centred on `y`.
    pub fn text(ui: &egui::Ui, span: Rangef, y: f32, value: &str, font: FontId, colour: Color32, align: Align2) {
        if value.is_empty() || span.max <= span.min {
            return;
        }
        let mut job = LayoutJob::simple_singleline(value.to_owned(), font, colour);
        job.wrap = TextWrapping::truncate_at_width(span.max - span.min);
        let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        let x = match align.x() {
            Align::Min => span.min,
            Align::Max => span.max,
            Align::Center => span.center(),
        };
        let anchored = align.anchor_size(egui::pos2(x, y), galley.size());
        ui.painter().galley(anchored.min, galley, colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(id: i64, name: &str, keywords: &str, active: bool, hits: i64, last_hit: &str) -> Topic {
        Topic {
            id,
            name: name.to_string(),
            keywords: keywords.to_string(),
            active,
            hits,
            last_hit: last_hit.to_string(),
        }
    }

    /// The columns fill the width they are given, in order, with a gap between them: a
    /// cell may not be drawn over its neighbour, however narrow the window gets.
    #[test]
    fn the_four_columns_fill_their_width_and_never_overlap() {
        // 632 px is the narrowest page the window allows: an 860 px window, less the
        // 188 px rail and the page's own 40 px margins (`src/main.rs`, `src/app.rs`).
        for width in [632.0_f32, 900.0, 1388.0] {
            let spans = table::spans(0.0, width);
            for pair in spans.windows(2) {
                assert!(pair[0].max < pair[1].min, "{width}: {pair:?} overlap or touch");
            }
            assert!(spans.iter().all(|span| span.min >= 0.0 && span.max <= width), "{width}: {spans:?}");
            assert_eq!(spans[3].max, width - 8.0, "{width}: the last column hugs the right pad");
            let name = spans[0].max - spans[0].min;
            assert!(name >= 200.0, "{width}: the name got {name} px");
        }
    }

    /// The three named columns keep their size when the window narrows; the name is what
    /// gives way, so a count and a time never become unreadable.
    #[test]
    fn a_narrower_window_shortens_the_name_and_not_the_numbers() {
        let wide = table::spans(0.0, 1388.0);
        let narrow = table::spans(0.0, 632.0);
        for column in 1..4 {
            let (was, now) = (wide[column], narrow[column]);
            assert_eq!(was.max - was.min, now.max - now.min, "column {column} moved");
        }
        assert!(narrow[0].max - narrow[0].min < wide[0].max - wide[0].min);
    }

    /// The row: one word for the state, the count grouped, and "never" for a stamp the
    /// archive did not write.
    #[test]
    fn a_row_reports_the_state_the_count_and_the_last_hit() {
        let watching = topic(1, "Chip supply", "chip, export", true, 1204, "");
        let row = line(&watching);
        assert_eq!((row.name.as_str(), row.state, row.on), ("Chip supply", "watching", true));
        assert_eq!(row.hits, "1 204", "a count is grouped like every other count");
        assert_eq!(row.last, NEVER);

        let paused = topic(2, "", "rail", false, 0, "2026-11-21T10:25:12Z");
        let row = line(&paused);
        assert_eq!((row.name.as_str(), row.state, row.on), ("topic 2", "paused", false));
        assert_eq!(row.hits, "0");
        assert_eq!(row.last, "21 Nov", "an old hit is a day, not a time");
    }

    /// The detail carries what the columns cannot say: the terms themselves.
    #[test]
    fn an_open_row_says_the_terms_it_matches() {
        let listed = topic(1, "Chip supply", "chip, semiconductor, export", true, 12, "");
        assert_eq!(terms(&listed), "Terms: chip, semiconductor, export");
        let bare = topic(2, "Untitled", "  ", true, 0, "");
        let detail = terms(&bare);
        // the page may not paint the opening words of its own empty state
        assert!(!detail.contains("No "), "{detail}");
        assert_eq!(detail, "This topic has no terms stored, so nothing matches it.");
    }

    /// The empty state is a sentence that names the page and the next step.
    #[test]
    fn the_empty_state_is_a_sentence_that_says_what_to_do() {
        assert!(EMPTY.starts_with("No topic yet"), "{EMPTY}");
        assert!(EMPTY.ends_with('.'), "{EMPTY}");
        assert!(EMPTY.len() > 24, "{EMPTY}");
        assert!(EMPTY.contains("research pass"), "the next step is named: {EMPTY}");
    }

    /// One sentence at the top, short enough to stay a line at the default width.
    #[test]
    fn the_page_says_what_a_topic_is_in_one_line() {
        assert!(WHAT.len() <= 90, "{} characters: {WHAT}", WHAT.len());
        assert!(WHAT.ends_with('.'));
    }

    /// What one page painted: every string and where, and every horizontal rule, which
    /// is where the table's own width is read from.
    #[derive(Default)]
    struct Painted {
        text: Vec<(String, egui::Rect)>,
        rules: Vec<egui::Rangef>,
    }

    /// Two frames of the page with no window in sight, because the first lays the table
    /// out and the second paints it where it landed — the same reason `tests/smoke.rs`
    /// draws three.
    fn painted(context: &egui::Context, topics: &[Topic]) -> Painted {
        let screen = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let mut out = Painted::default();
        for _ in 0..2 {
            let output = context.run_ui(screen.clone(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| show(ui, topics));
            });
            out = Painted::default();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut out);
            }
            // egui 0.37 asserts in debug when a `FullOutput` is dropped with texture
            // deltas still in it; a headless frame has nothing to upload them to.
            output.drop_without_applying_deltas();
        }
        out
    }

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
            // a horizontal hairline is a row's edge or the header's: either way it spans
            // the table's own width, which is what the columns are measured against
            egui::Shape::LineSegment { points, .. } if (points[0].y - points[1].y).abs() < 0.5 => {
                out.rules.push(egui::Rangef::new(
                    points[0].x.min(points[1].x),
                    points[0].x.max(points[1].x),
                ));
            }
            _ => {}
        }
    }

    impl Painted {
        fn any(&self, value: &str) -> bool {
            self.text.iter().any(|(text, _)| text == value)
        }

        /// The first cell whose text starts with `start`, and where it is.
        fn starting(&self, start: &str) -> Option<egui::Rect> {
            self.text
                .iter()
                .find(|(text, _)| text.starts_with(start))
                .map(|(_, rect)| *rect)
        }

        /// Where the cell painted exactly `value` is drawn: the point a pointer aims at.
        fn at(&self, value: &str) -> egui::Rect {
            self.cell(value)
        }

        fn stamped(&self, start: &str) -> bool {
            self.text.iter().any(|(text, _)| text.starts_with(start))
        }

        fn cell(&self, value: &str) -> egui::Rect {
            self.text
                .iter()
                .find(|(text, _)| text == value)
                .unwrap_or_else(|| {
                    panic!(
                        "{value:?} was never painted; the page painted: {:?}",
                        self.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
                    )
                })
                .1
        }
    }

    /// The table, drawn: a header above the rows, four cells on one line per row, in
    /// column order, and every cell inside the column it belongs to.
    #[test]
    fn the_page_paints_a_header_and_one_line_per_row() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let topics = vec![
            topic(1, "Chip supply", "chip, semiconductor, export", true, 1204, ""),
            topic(2, "Alpine freight", "rail, freight", false, 3, ""),
        ];
        let table = painted(&context, &topics);

        for label in ["name", "state", "hits", "last activity"] {
            table.cell(label);
        }
        for value in ["Chip supply", "Alpine freight", "watching", "paused", "1 204", "3", NEVER] {
            table.cell(value);
        }

        let name = table.cell("Chip supply");
        let state = table.cell("watching");
        let hits = table.cell("1 204");
        let last = table.cell(NEVER);
        // one row is one line: the four cells share a baseline, in column order
        assert!(
            (name.center().y - last.center().y).abs() <= 1.0,
            "the cells of one row are not on one line"
        );
        assert!(name.right() < state.left(), "the name runs into the state");
        assert!(state.right() < hits.left(), "the state runs into the count");
        assert!(hits.right() <= last.left(), "the count runs into the time");
        // the header stands above the first row, over the same columns: a header that
        // drifts from the rows is the failure this is for
        let header = table.cell("name");
        assert!(header.bottom() < name.top(), "the header is not above the rows");
        // The count is right-aligned in its column, so the header is compared on the edge
        // each column actually aligns to. The state column is left out on purpose: it
        // leads with the dot, so its word starts a dot and a gap to the right of the
        // heading that names the column.
        for (label, cell, right) in
            [("name", name, false), ("hits", hits, true), ("last activity", last, false)]
        {
            let over = table.cell(label);
            let (was, now) = if right { (over.right(), cell.right()) } else { (over.left(), cell.left()) };
            assert!(
                (was - now).abs() <= 1.0,
                "the header {label:?} is not over its column: {over:?} vs {cell:?}"
            );
        }
        // and no cell leaves the table's own width, read from the hairline under the
        // header rather than assumed
        let rule = *table.rules.first().expect("the table painted no rule");
        let spans = table::spans(rule.min, rule.max - rule.min);
        for (rect, span) in [(name, spans[0]), (state, spans[1]), (hits, spans[2]), (last, spans[3])] {
            assert!(
                rect.left() >= span.min - 0.5 && rect.right() <= span.max + 0.5,
                "{rect:?} is outside its column {span:?}"
            );
        }
        // a page with rows may not paint the opening words of its own empty state
        assert!(
            table.text.iter().all(|(text, _)| !text.starts_with("No ")),
            "a full page painted its empty state"
        );
    }

    /// A row that is open paints the terms under itself, and only then: with nothing open
    /// the page explains what a click does instead.
    #[test]
    fn an_open_row_paints_its_detail_and_a_shut_page_paints_the_hint() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let topics = vec![topic(1, "Chip supply", "chip, semiconductor, export", true, 12, "")];

        let shut = painted(&context, &topics);
        assert!(shut.any(HINT), "the page does not say what a click does");
        assert!(!shut.stamped("Terms: "), "a shut page painted a detail");

        table::remember(&context, PAGE, Some(1));
        let open = painted(&context, &topics);
        assert!(
            open.any("Terms: chip, semiconductor, export"),
            "an open row did not paint the terms behind it; it painted: {:?}",
            open.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert!(!open.any(HINT), "the hint survived the detail");
        // the detail hangs under its row, not over it
        let detail = open.cell("Terms: chip, semiconductor, export");
        assert!(
            detail.top() >= open.cell("Chip supply").bottom(),
            "the detail is painted over its own row"
        );
    }

    /// A table does not wrap: pressing Down on the last row stays there.
    #[test]
    fn the_keyboard_stops_at_the_last_row() {
        assert_eq!(landed(1, 1, 3), 2);
        assert_eq!(landed(2, 1, 3), 2);
        assert_eq!(landed(0, -1, 3), 0);
        assert_eq!(landed(2, -2, 3), 0, "a bigger step still stops at the first row");
        assert_eq!(landed(0, 1, 0), 0, "an empty list has nowhere to go");
    }

    // ------------------------------------------------------------------ the story band

    /// The complaint, word for word: "nothing happens when I click one of those topics". A
    /// press opens the row and its stories; a second press shuts both. The stories are the
    /// band's own rows, not a count and not a promise.
    #[test]
    fn clicking_a_topic_opens_its_stories_and_clicking_again_shuts_them() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let topics = vec![
            topic(1, "Chip supply", "chip, export", true, 2, ""),
            topic(2, "Alpine freight", "rail", true, 0, ""),
        ];
        let rows = [
            story(11, "Wafer prices fall", "Wire", "2026-11-21T10:00:00Z"),
            story(12, "Export ban widens", "Wire", "2026-11-20T10:00:00Z"),
        ];
        let stories = Stories::of(1, &rows);

        let (shut, _) = frame(&context, &topics, stories, &queue, Vec::new());
        assert!(!shut.any("Wafer prices fall"), "a shut row painted its stories");
        assert!(shut.any(HINT), "the page does not say what a press does");

        let at = shut.at("Chip supply").center();
        frame(&context, &topics, stories, &queue, click(at));
        let (open, _) = frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), Some(1), "the press did not open the row");
        for title in ["Wafer prices fall", "Export ban widens"] {
            assert!(
                open.any(title),
                "an open row did not paint {title:?}; it painted: {:?}",
                open.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
            );
        }
        // the stories hang under the row they belong to, newest first
        let row = open.at("Chip supply");
        let first = open.at("Wafer prices fall");
        let second = open.at("Export ban widens");
        assert!(first.top() > row.bottom(), "the stories are painted over their row");
        assert!(first.top() < second.top(), "the stories are not newest first");

        // a second press on the same row shuts it: the stories go with it
        let at = open.at("Chip supply").center();
        frame(&context, &topics, stories, &queue, click(at));
        let (again, _) = frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), None, "a second press did not shut the row");
        assert!(!again.any("Wafer prices fall"), "the stories survived the row");
        assert!(again.any(HINT));

        // another row's stories are never painted under this one
        let at = again.at("Alpine freight").center();
        frame(&context, &topics, stories, &queue, click(at));
        let (other, _) = frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), Some(2));
        assert!(
            !other.any("Wafer prices fall"),
            "the open row paints another topic's stories"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every story row under an open topic opens *that* story — not the newest one, not the
    /// row's own toggle — and the link under the list opens the newest of them.
    #[test]
    fn every_story_row_under_a_topic_opens_that_story() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let topics = vec![topic(1, "Chip supply", "chip", true, 3, "")];
        let rows = [
            story(11, "Wafer prices fall", "Wire", "2026-11-21T10:00:00Z"),
            story(12, "Export ban widens", "Wire", "2026-11-20T10:00:00Z"),
            story(13, "Fab output recovers", "Wire", "2026-11-19T10:00:00Z"),
        ];
        let stories = Stories::of(1, &rows);
        remember_open(&context, Some(1));
        let (open, _) = frame(&context, &topics, stories, &queue, Vec::new());

        for row in &rows {
            let at = open.at(&row.title).center();
            let (_, asked) = frame(&context, &topics, stories, &queue, click(at));
            assert_eq!(
                asked,
                Some(row.id),
                "a press on {:?} asked for {asked:?}",
                row.title
            );
            assert_eq!(open_topic(&context), Some(1), "a press on a story shut the row it is in");
        }
        // "Show these in Articles" is the newest of the list, not the row the pointer last
        // touched: it is the answer to "show me *these*".
        let at = open.at(OPEN_IN_ARTICLES).center();
        let (_, asked) = frame(&context, &topics, stories, &queue, click(at));
        assert_eq!(asked, Some(11), "the link under the list opened {asked:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The count on a row and the stories it opens are one number. When the shell hands over
    /// everything the topic matched, the list is that long and says nothing; when it hands
    /// over fewer — another window, or the shell's own cap — the band says which is which
    /// instead of printing a count over a shorter list.
    #[test]
    fn the_count_on_a_topic_row_is_the_stories_the_row_opens() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();

        let topics = vec![topic(1, "Chip supply", "chip", true, 2, "")];
        let two = [
            story(11, "Story one", "Wire", "2026-11-21T10:00:00Z"),
            story(12, "Story two", "Wire", "2026-11-20T10:00:00Z"),
        ];
        remember_open(&context, Some(1));
        let (open, _) = frame(&context, &topics, Stories::of(1, &two), &queue, Vec::new());
        let listed = open
            .text
            .iter()
            .filter(|(text, _)| text.starts_with("Story "))
            .count();
        assert_eq!(listed, 2, "the row opens {listed} of the 2 stories it counts");
        assert_eq!(line(&topics[0]).hits, listed.to_string(), "the count and the list differ");
        assert!(
            open.starting("Showing").is_none(),
            "a complete list was called incomplete: {:?}",
            open.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );

        // The shipped archive's own shape: a topic that matched far more than the shell reads
        // hands over 40 rows while its row counts 1 204.
        let many: Vec<Article> = (0..40)
            .map(|index| {
                story(
                    100 + index,
                    &format!("Story {index}"),
                    "Wire",
                    "2026-11-21T10:00:00Z",
                )
            })
            .collect();
        let topics = vec![topic(1, "Chip supply", "chip", true, 1204, "")];
        let (capped, _) = frame(&context, &topics, Stories::of(1, &many), &queue, Vec::new());
        let shown = showing(40, 1204);
        assert_eq!(shown, "Showing the newest 40 of 1 204 matched stories.");
        assert!(
            capped.any(&shown),
            "the band does not reconcile the count with the list; it painted: {:?}",
            capped.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(
            capped.text.iter().filter(|(text, _)| text.starts_with("Story ")).count(),
            40
        );
        // the sentence sits above the list it is about
        assert!(capped.at(&shown).bottom() < capped.at("Story 0").top());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Typing a topic and pressing the action queues exactly one `topic_add` request carrying
    /// the reader's own words — both keys, unbent — and says so; an empty field writes
    /// nothing.
    #[test]
    fn adding_a_topic_queues_one_topic_add_request_with_the_readers_own_words() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let topics = vec![topic(1, "Chip supply", "chip", true, 2, "")];

        let (first, _) = frame(&context, &topics, Stories::none(), &queue, Vec::new());
        assert!(first.any(ADD_LABEL), "the page paints no way to add a topic");
        let field = first.at(ADD_HINT).center();
        frame(&context, &topics, Stories::none(), &queue, click(field));
        let words = "rare earths and magnets";
        let (typed, _) = frame(
            &context,
            &topics,
            Stories::none(),
            &queue,
            vec![egui::Event::Text(words.to_string())],
        );
        // What the field holds is the state the submit reads; egui paints a text edit one
        // frame behind the event that changed it.
        let draft = context
            .data(|data| data.get_temp::<String>(egui::Id::unique(("atlas-topic-draft", PAGE))))
            .unwrap_or_default();
        assert_eq!(draft, words, "the field did not take the typed words");

        let at = typed.at(ADD_ACTION).center();
        frame(&context, &topics, Stories::none(), &queue, click(at));
        let (after, _) = frame(&context, &topics, Stories::none(), &queue, Vec::new());

        let queued_rows = queued(&queue);
        assert_eq!(queued_rows.len(), 1, "the press queued {}: {queued_rows:?}", queued_rows.len());
        assert_eq!(queued_rows[0].0, "topic_add");
        let payload: serde_json::Value = serde_json::from_str(&queued_rows[0].1).expect("valid JSON");
        assert_eq!(payload["query"], words, "the question is not the reader's sentence");
        assert_eq!(payload["name"], words, "one field is both name and question");
        assert!(
            after.starting("asked the agent to add a topic about").is_some(),
            "the page did not say what it did: {:?}",
            after.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert!(!after.any(words), "the field kept the words that were already sent");

        // A press with nothing typed is a sentence, not a second row.
        let at = after.at(ADD_ACTION).center();
        frame(&context, &topics, Stories::none(), &queue, click(at));
        let (empty, _) = frame(&context, &topics, Stories::none(), &queue, Vec::new());
        assert!(empty.any(ADD_EMPTY), "an empty field wrote something");
        assert_eq!(queued(&queue).len(), 1, "an empty field queued a request");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A queue that cannot be written is one sentence saying why, with the reader's words
    /// still in the field — never a silent nothing and never a request that pretends to have
    /// been sent.
    #[test]
    fn a_refused_topic_request_says_why_in_one_sentence() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        // A directory where the queue's file should be: SQLite cannot open it as a database,
        // so the refusal comes from the file system and not from anything in the page.
        std::fs::create_dir_all(dir.join("data").join("requests.db")).expect("a blocker directory");
        let topics = vec![topic(1, "Chip supply", "chip", true, 2, "")];

        let (first, _) = frame(&context, &topics, Stories::none(), &queue, Vec::new());
        let field = first.at(ADD_HINT).center();
        frame(&context, &topics, Stories::none(), &queue, click(field));
        let words = "rare earths";
        let (typed, _) = frame(
            &context,
            &topics,
            Stories::none(),
            &queue,
            vec![egui::Event::Text(words.to_string())],
        );
        let at = typed.at(ADD_ACTION).center();
        frame(&context, &topics, Stories::none(), &queue, click(at));
        let (after, _) = frame(&context, &topics, Stories::none(), &queue, Vec::new());

        let said: Vec<&str> = after
            .text
            .iter()
            .map(|(text, _)| text.as_str())
            .filter(|text| text.starts_with("the window could not"))
            .collect();
        assert_eq!(said.len(), 1, "the refusal is {} sentences: {said:?}", said.len());
        let said = said[0];
        assert!(said.contains("request queue"), "the sentence does not name the queue: {said}");
        assert!(said.ends_with('.'), "not a sentence: {said}");
        // one sentence: one full stop, and no filesystem path in a window
        assert_eq!(said.matches('.').count(), 1, "more than one sentence: {said}");
        assert!(!said.contains('/'), "the refusal paints a path: {said}");
        assert!(
            after.any(words),
            "a refused write threw the reader's words away: {:?}",
            after.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert!(!queue.is_file(), "a refused write left a queue file behind");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The keyboard alone: arrows move the row the keys act on, Enter opens and shuts it, Esc
    /// shuts it from anywhere on the page.
    #[test]
    fn the_keyboard_moves_opens_and_closes() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let topics = vec![
            topic(1, "Chip supply", "chip", true, 2, ""),
            topic(2, "Alpine freight", "rail", true, 1, ""),
        ];
        let chip = [story(11, "Wafer prices fall", "Wire", "2026-11-21T10:00:00Z")];
        let rail = [story(21, "Alpine tunnel reopens", "Wire", "2026-11-20T10:00:00Z")];
        let stories = Stories::of(2, &rail);

        // A press puts the keyboard on that row (the row is the widget; a click focuses it).
        let (first, _) = frame(&context, &topics, Stories::of(1, &chip), &queue, Vec::new());
        let at = first.at("Chip supply").center();
        frame(&context, &topics, Stories::of(1, &chip), &queue, click(at));
        assert_eq!(
            context.memory(|memory| memory.focused()),
            Some(table::row_id(PAGE, 1)),
            "the pressed row did not take the keyboard"
        );

        // Down moves to the second row, which is then the row Enter acts on.
        frame(&context, &topics, stories, &queue, vec![key(egui::Key::ArrowDown)]);
        assert_eq!(
            context.memory(|memory| memory.focused()),
            Some(table::row_id(PAGE, 2)),
            "the down arrow did not move to the next row"
        );
        let (_, _) = frame(&context, &topics, stories, &queue, vec![key(egui::Key::Enter)]);
        let (opened, _) = frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), Some(2), "Enter did not open the row it is on");
        assert!(opened.any("Alpine tunnel reopens"), "the open row painted no stories");

        // Enter again shuts it, and Esc shuts it from anywhere.
        frame(&context, &topics, stories, &queue, vec![key(egui::Key::Enter)]);
        frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), None, "a second Enter did not shut the row");
        frame(&context, &topics, stories, &queue, vec![key(egui::Key::Enter)]);
        frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), Some(2), "Enter could not open the row again");
        frame(&context, &topics, stories, &queue, vec![key(egui::Key::Escape)]);
        let (shut, _) = frame(&context, &topics, stories, &queue, Vec::new());
        assert_eq!(open_topic(&context), None, "Esc did not shut the open row");
        assert!(!shut.any("Alpine tunnel reopens"), "the stories survived Esc");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Nothing to show, three different nothings: an archive with no topics, a topic nothing
    /// has matched yet, and a topic whose matches are outside the window. Each says which it
    /// is, and the first still offers the one thing the reader can do here.
    #[test]
    fn the_page_says_what_to_do_when_there_is_nothing_to_show() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();

        let (empty, _) = frame(&context, &[], Stories::none(), &queue, Vec::new());
        assert!(
            empty.starting("No topic yet").is_some(),
            "an archive with no topics painted no empty state: {:?}",
            empty.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(EMPTY.starts_with("No topic yet"), true);
        assert!(
            !empty.any("name"),
            "an empty page painted a table header: {:?}",
            empty.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert!(
            empty.any(ADD_LABEL) && empty.any(ADD_HINT),
            "the empty page offers no way to ask for a topic"
        );

        // A topic the archive has never assigned a story to.
        let untouched = vec![topic(5, "Deep sea mining", "nodules", true, 0, "")];
        remember_open(&context, Some(5));
        let (never, _) = frame(&context, &untouched, Stories::of(5, &[]), &queue, Vec::new());
        assert!(never.any(NOTHING_YET), "a topic nothing matched says the wrong thing");
        assert!(!never.any(NONE_IN_WINDOW), "the two nothings are not told apart");

        // A topic that matched, but not inside this window.
        let elsewhere = vec![topic(6, "Deep sea mining", "nodules", true, 7, "")];
        remember_open(&context, Some(6));
        let (outside, _) = frame(&context, &elsewhere, Stories::of(6, &[]), &queue, Vec::new());
        assert!(outside.any(NONE_IN_WINDOW), "a windowed-out topic says the wrong thing");
        assert!(!outside.any(NOTHING_YET), "the two nothings are not told apart");

        // Nothing read at all is not a claim about the archive: no band, no sentence.
        let (unread, _) = frame(&context, &elsewhere, Stories::none(), &queue, Vec::new());
        assert!(!unread.any(NONE_IN_WINDOW) && !unread.any(NOTHING_YET));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The shipped archive, through the page: the complaint was about *these* rows. The first
    /// topic's own stories — read here the way the shell reads them — are painted under its
    /// row, and the count on the row is reconciled with what the list holds.
    #[test]
    fn the_shipped_archive_row_opens_its_own_stories() {
        let file = crate::assets::archive();
        let Ok(archive) = crate::data::Archive::open(&file) else {
            println!("skipped: no readable archive at {}", file.display());
            return;
        };
        let topics = archive.topics();
        let Some(topic) = topics.first() else {
            println!("skipped: the archive holds no topic");
            return;
        };
        let rows = archive.topic_articles(topic.id, 72, 40);
        assert!(!rows.is_empty(), "the shipped topic {:?} opens nothing in 72 h", topic.name);

        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        remember_open(&context, None);
        let stories = Stories::of(topic.id, &rows);
        let (shut, _) = frame(&context, &topics, stories, &queue, Vec::new());
        let at = shut.at(&line(topic).name).center();
        frame(&context, &topics, stories, &queue, click(at));
        let (open, _) = frame(&context, &topics, stories, &queue, Vec::new());

        assert_eq!(open_topic(&context), Some(topic.id), "the shipped row did not open");
        for story in &rows {
            assert!(
                open.any(&story.title),
                "the open row does not paint {:?}; it painted: {:?}",
                story.title,
                open.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
            );
        }
        let count = line(topic).hits;
        assert_eq!(count, nf(topic.hits), "the row's count is not the archive's");
        if (rows.len() as i64) < topic.hits {
            assert!(
                open.any(&showing(rows.len(), topic.hits)),
                "the count {count} and the {} stories below it are not reconciled",
                rows.len()
            );
        }
        println!(
            "shipped topic {:?}: row counts {count}, {} stories painted under it",
            topic.name,
            rows.len()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// One key press, as the platform sends it.
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    // ------------------------------------------------------------------ the request link

    /// A queue of the test's own, in a directory it deletes: the page's link writes a real
    /// row (that is the thing worth testing), but never the checkout's `data/`.
    fn queue() -> (std::path::PathBuf, std::path::PathBuf) {
        let name = format!(
            "atlas-topics-queue-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(name.replace(['(', ')', ' '], "-"));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("data").join("requests.db");
        (dir, file)
    }

    /// The rows a test's queue holds, newest last: `(kind, payload)`.
    fn queued(file: &std::path::Path) -> Vec<(String, String)> {
        if !file.exists() {
            return Vec::new();
        }
        let connection = rusqlite::Connection::open_with_flags(
            file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("the queue the page just wrote");
        let mut statement = connection
            .prepare("SELECT kind, payload FROM requests ORDER BY id")
            .expect("the table the page created");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("a query")
            .map(|row| row.expect("a row"))
            .collect()
    }

    /// One story as the archive hands it over: only the fields a row paints are filled.
    fn story(id: i64, title: &str, source: &str, published: &str) -> Article {
        Article {
            id,
            title: title.to_string(),
            source: source.to_string(),
            published: published.to_string(),
            summary: String::new(),
            content: String::new(),
            country: String::new(),
            url: String::new(),
            full_text: false,
        }
    }

    /// One headless frame of the page, with the stories the shell would have read and the
    /// queue a request link writes to. Returns what the frame painted and the story the
    /// frame asked to open.
    fn frame(
        context: &egui::Context,
        topics: &[Topic],
        stories: Stories<'_>,
        queue: &std::path::Path,
        events: Vec<egui::Event>,
    ) -> (Painted, Option<i64>) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        input.events = events;
        let mut out = Painted::default();
        let mut asked = None;
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                asked = show_with_queue(ui, topics, stories, queue);
            });
        });
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        output.drop_without_applying_deltas();
        (out, asked)
    }

    /// A press and release on `at`: the two frames a real pointer sends.
    fn click(at: egui::Pos2) -> Vec<egui::Event> {
        let mut events = vec![egui::Event::PointerMoved(at)];
        for pressed in [true, false] {
            events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        events
    }

    /// One press of the control the page labels `label`: the shapes of one frame find it,
    /// the next two press and release on it, and the fourth paints what the page says
    /// afterwards.
    ///
    /// The same three-frame dance `src/app.rs` uses for the rail, for the same reason: a
    /// click arrives as a press frame and a release frame, and the sentence a press produces
    /// is drawn in the frame after it.
    fn press(
        context: &egui::Context,
        topics: &[Topic],
        queue: &std::path::Path,
        label: &str,
    ) -> Painted {
        let (first, _) = frame(context, topics, Stories::none(), queue, Vec::new());
        let at = first.cell(label).center();
        frame(context, topics, Stories::none(), queue, click(at));
        frame(context, topics, Stories::none(), queue, Vec::new()).0
    }

    /// The action under an open topic queues one `research` request and then says what it
    /// did, in one line — and reads nothing: the archive this page draws is stale by exactly
    /// one click, which is the honest state after an intent.
    #[test]
    fn the_open_topic_queues_one_research_request_and_says_so_in_one_line() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let topics = vec![topic(1, "chips", "chip, semiconductor", true, 12, "")];
        table::remember(&context, PAGE, Some(1));

        // The control exists, with the page's own words.
        let first = press(&context, &topics, &queue, RESEARCH_NOW);
        assert!(
            first.any("asked the agent to look into chips"),
            "the page did not say what it did; it painted: {:?}",
            first.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        // One row, of the right kind, with the payload the agent will read.
        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press queued {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "research");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("valid JSON");
        assert_eq!(payload["topic"], "chips");
        assert_eq!(payload["max_steps"], 6);
        // A second press is a second request: the link is a button, not a state.
        let second = press(&context, &topics, &queue, RESEARCH_NOW);
        assert_eq!(queued(&queue).len(), 2, "a second press did not queue a second request");
        assert!(second.any("asked the agent to look into chips"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The failure path: a queue that cannot be written is a sentence the reader can act on,
    /// painted where the success line would be — never a panic and never a silent nothing.
    #[test]
    fn a_queue_that_cannot_be_written_is_a_sentence_in_the_page() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        // A *directory* where the queue's file should be: SQLite cannot open a directory as a
        // database, so the failure comes from the file system rather than from anything in the
        // page — and `tests/fresh_machine.rs` can keep forbidding every file-write shape
        // outside the queue's own module, which it only can if a view's test writes nothing.
        std::fs::create_dir_all(dir.join("data").join("requests.db")).expect("a blocker directory");
        let topics = vec![topic(1, "chips", "chip", true, 3, "")];
        table::remember(&context, PAGE, Some(1));

        let after = press(&context, &topics, &queue, RESEARCH_NOW);
        let said = after
            .text
            .iter()
            .find(|(text, _)| text.starts_with("the window could not"))
            .map(|(text, _)| text.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "the page did not say why it could not; it painted: {:?}",
                    after.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
                )
            });
        assert!(said.contains("request queue"), "{said}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
