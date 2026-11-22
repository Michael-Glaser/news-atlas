//! Monitors: a saved question, its cadence, what it collected, and when it last ran.
//!
//! The same table as Topics — four columns, a header that stays put, a row that opens on
//! a click or on Enter — because a reader should not have to learn a second layout to
//! read the second list. What differs is what a row *is*: a monitor asks the model a
//! question again on a cadence, and every new hit becomes an alert, so the detail under
//! an open row is the question, the cadence, the monitor's own state — and the stories it
//! actually collected, newest first, each one a row a reader can click to open it.
//!
//! That last part is what "interactive" means here, and it is why the page takes
//! [`MonitorStories`]: a page cannot read the archive (`src/app.rs` owns the one
//! connection and its one-measured-read rhythm), so the shell reads the stories of the
//! row it knows is open and hands them in. The page reports back the story a reader asked
//! to open, and `app.rs` opens it. Nothing is cached here and nothing is queried twice.
//!
//! Read-only, like every page: `data::Archive` opens the file `SQLITE_OPEN_READ_ONLY`,
//! so the state column reports the archive's `active` flag and nothing here flips it.
//!
//! The column metrics, the header, the row and the detail are [`super::topics::table`]'s
//! — see that module for why the shared half of both pages lives in `topics.rs`.

use super::topics::table;
use super::{clip, heading, nf, when};
use crate::data::{Article, Monitor};
use crate::theme;

/// One sentence: what a monitor is in this app. Under 80 characters, so it stays one
/// line at the window's default width.
const WHAT: &str = "A monitor is a saved question, re-asked on a cadence; new hits become alerts.";

/// Painted only when the archive holds no monitor. Begins with "No monitor yet" and ends
/// with a full stop: the smoke test reads a page's empty state out of the page itself.
pub const EMPTY: &str = "No monitor yet. A monitor is a question the model re-asks on a cadence, \
and every new hit becomes an alert. This window only reads — save one with atlas-monitor add and it \
appears here.";

/// Under the table while no row is open: the affordance, and what `hits` counts here —
/// which is not what it counts on Topics, because a monitor counts alerts.
const HINT: &str = "Select a row to read its question. Hits are the alerts it raised.";

/// The word for a stamp the archive never wrote (`atlas-store` leaves `last_run` empty
/// until a check actually runs).
const NEVER: &str = "never";

/// A year in minutes: the point past which a cadence is not a schedule a reader can wait
/// for. A year and a day is still a cadence the sentence counts; `i64::MAX` is not.
const A_YEAR: i64 = 365 * 24 * 60;

const WATCHING: &str = "watching";
const PAUSED: &str = "paused";

/// The action under an open monitor: ask the supervised agent to run it now.
///
/// This window cannot run a monitor — nothing here collects, and `data::Archive` is opened
/// `SQLITE_OPEN_READ_ONLY` — so the button queues one `monitor_run` request
/// (`src/requests.rs`) naming the row's id and says what it did in one line.
const RUN_NOW: &str = "Run this now";
const RUN_HOVER: &str = "Ask the agent to run this monitor now, instead of waiting for its cadence";

/// The page header's own action: add what the reader typed as a monitor.
///
/// One field, and the field *is* the monitor: the sentence the reader types is the question
/// the monitor asks, so it is sent as both `name` and `query`. A second, invented name would
/// be the window writing words nobody said.
const ADD_LABEL: &str = "Add a monitor";
const ADD_HINT: &str = "what should it watch for?";
const ADD_ACTION: &str = "Ask the agent to add it";
const ADD_HOVER: &str = "Queue a request for a monitor with exactly these words";
/// What the header says when the reader pressed the action with nothing typed. A sentence
/// in the interface's voice, and it asks for nothing (`requests.rs` would refuse the empty
/// payload too; this one says why to the reader).
const ADD_EMPTY: &str = "Type what the monitor should watch for first.";

/// The header's own slot in the page's memory: rows are numbered from one, so zero cannot
/// be a row and is the header (`table::remember_said`).
const HEADER: i64 = 0;

/// The page's own id: keyboard, scroll offset and row ids hang off it.
const PAGE: &str = "monitors";

/// Under an open row that collected nothing *ever*: `hits` counts every story a monitor
/// raised an alert for, so zero is a fact about the monitor, not about the window.
const NOTHING_YET: &str = "This monitor has not collected anything yet.";

/// Under an open row that has alerts but no story inside the window the shell read.
const NONE_IN_WINDOW: &str = "This monitor has no story inside the window.";

/// The one thing an open monitor cannot do, said plainly instead of hidden behind a
/// button that does nothing.
///
/// The window cannot delete a monitor for two reasons, and both are structural: the
/// archive is opened `SQLITE_OPEN_READ_ONLY` (`data::Archive::open`), and the queue takes
/// six kinds, none of them a deletion (`requests.rs:61`). The agent can — `atlas-monitor
/// delete` takes `{"id":N}` (`atlas-monitor/src/wire.rs:40`) — so the sentence names the
/// tool that can rather than leaving a reader to guess. A greyed-out button was the other
/// option: it looks like a control, answers a keyboard press with nothing, and cannot say
/// why, which is the defect this page is being polished out of.
const NO_DELETE: &str = "This window cannot delete a monitor: its archive is read-only. The \
agent removes one with atlas-monitor delete.";

/// The story row's two right-hand columns, on the table's own spacing so the eye reads
/// one column of dates down the page. Same widths as `topics.rs`'s story row, because
/// these are the same two facts (outlet, day) in the same place.
const STORY_GAP: f32 = crate::theme::space::MD;
const STORY_SOURCE_W: f32 = crate::theme::space::COLUMN;
const STORY_WHEN_W: f32 = crate::theme::space::COLUMN + crate::theme::space::SM;

/// The longest name a row will draw, and the longest query a detail will draw.
///
/// Both come out of a database this window does not write. A row is one line of fixed
/// height, so a name longer than the cell is the table's own problem (`table::text`
/// elides at the cell width) — but the *detail* is prose that wraps, and a 200 KB query
/// would be a 200 KB galley rebuilt every frame. These are lengths, not word lists: the
/// content is still whatever the archive stored.
const NAME_MAX: usize = 120;
const QUERY_MAX: usize = 600;

/// The stories the shell read for one monitor, and which monitor they belong to.///
/// The page does not query. `src/app.rs` owns the archive, so it reads the stories of the
/// monitor whose row is open — [`open_monitor`] is how it asks which one that is before
/// the frame — and hands them here with that monitor's id. The id is part of the value
/// because the two can disagree for one frame: the reader opens another row while the
/// rows in hand are still the previous row's, and drawing those under the new row would
/// attribute one monitor's stories to another. A mismatch is drawn as *nothing read* —
/// never as "this monitor has no stories".
///
/// `Copy` because the page walks the monitor rows in a loop and the open one draws the
/// same list once: an id and a slice are cheaper to copy than to borrow twice.
#[derive(Clone, Copy)]
pub struct MonitorStories<'a> {
    /// The monitor `rows` belong to; `None` when the shell read nothing.
    pub monitor: Option<i64>,
    /// That monitor's stories, newest first — the order the query answered in.
    pub rows: &'a [Article],
}

impl<'a> MonitorStories<'a> {
    /// Nothing read: no story list is drawn, and the page claims nothing about stories.
    pub fn none() -> MonitorStories<'a> {
        MonitorStories {
            monitor: None,
            rows: &[],
        }
    }

    /// The stories of one monitor, newest first.
    pub fn of(monitor: i64, rows: &'a [Article]) -> MonitorStories<'a> {
        MonitorStories {
            monitor: Some(monitor),
            rows,
        }
    }

    /// The rows to draw under `monitor`: its own, or `None` for "not read".
    fn under(&self, monitor: i64) -> Option<&'a [Article]> {
        match self.monitor {
            Some(id) if id == monitor => Some(self.rows),
            _ => None,
        }
    }
}

/// The monitor whose row is open, or `None`.
///
/// The open row lives in the context's own memory (see [`table::open`]), so a shell that
/// wants to read the stories *before* the frame can ask this and get the same answer the
/// page will draw with — that is what makes one read per frame enough.
pub fn open_monitor(ctx: &egui::Context) -> Option<i64> {
    table::open(ctx, PAGE)
}

/// Open, or close, a monitor's row from outside the page.
pub fn remember_open(ctx: &egui::Context, monitor: Option<i64>) {
    table::remember(ctx, PAGE, monitor);
}

/// The page, with the stories of the open monitor.
///
/// Returns the story a reader asked to open: a click on a story row under the open
/// monitor, or Enter/Space on the story row the keyboard is on. `None` means "no story
/// was asked for this frame" — the page is a pure function of `monitors` and `stories`,
/// so nothing else is reported and nothing is remembered that the caller cannot see.
pub fn show_with(
    ui: &mut egui::Ui,
    monitors: &[Monitor],
    stories: MonitorStories<'_>,
) -> Option<i64> {
    // The shipped window queues into its one writable file; a caller that wants another
    // target (a test, so a headless frame never touches `data/`) calls [`show_with_queue`].
    show_with_queue(ui, monitors, stories, &crate::requests::queue_path())
}

/// The page, with the queue its request links write to.
///
/// The path is a parameter because this is where the window's one write happens:
/// `app.rs` hands the queue the window opened with, and a test hands a temporary one.
pub fn show_with_queue(
    ui: &mut egui::Ui,
    monitors: &[Monitor],
    stories: MonitorStories<'_>,
    queue: &std::path::Path,
) -> Option<i64> {
    heading(ui, "Monitors", WHAT);
    // The header's own action, above the table and below the title: a reader with something
    // to watch for should not have to open a row to ask for it. It is drawn before the
    // table's own measurements, so the rows get what is actually left.
    add_a_monitor(ui, queue);
    if monitors.is_empty() {
        table::empty(ui, EMPTY);
        return None;
    }

    let width = table::width(ui);
    table::header(ui, width, ["name", "state", "hits", "last activity"]);

    let ids: Vec<egui::Id> = monitors
        .iter()
        .map(|monitor| table::row_id(PAGE, monitor.id))
        .collect();
    let mut open = open_monitor(ui.ctx());
    // The row the arrow keys move to, applied after the loop: the row that has the
    // keyboard asks for the next one, and the request has to outlive its own frame.
    let mut next = None;
    // The story this frame's click asked for, if any.
    let mut asked: Option<i64> = None;
    let height = table::rows_height(ui);
    egui::ScrollArea::vertical()
        .id_salt("monitors-rows")
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (index, monitor) in monitors.iter().enumerate() {
                let response =
                    table::row(ui, width, ids[index], &line(monitor), open == Some(monitor.id));
                if table::activate(ui, &response) {
                    open = if open == Some(monitor.id) { None } else { Some(monitor.id) };
                }
                let step = table::step(ui, &response);
                if step != 0 {
                    next = Some(landed(index, step, monitors.len()));
                }
                if open == Some(monitor.id) {
                    asks_and_run(ui, width, monitor, queue);
                    asked = stories_under(ui, width, monitor, stories).or(asked);
                }
                if response.gained_focus() {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });

    if let Some(index) = next {
        table::focus(ui, ids[index]);
    }
    if open.is_some() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        open = None;
    }
    remember_open(ui.ctx(), open);
    if open.is_none() {
        table::caption(ui, HINT);
    }
    asked
}

/// The page without a story list: the look it had before this page could open anything.
///
/// The shell calls [`show_with`]; this wrapper exists so a caller that has no archive in
/// its hand (the smoke test draws every page, and `app.rs`'s `Page::Monitors` arm until it
/// is wired) keeps the signature it has. An open monitor then shows its detail and no
/// story band at all — absent, not empty, because the page was never told.
pub fn show(ui: &mut egui::Ui, monitors: &[Monitor]) {
    let _ = show_with(ui, monitors, MonitorStories::none());
}

/// The open monitor's question, and the one thing the row can *do*.
///
/// The question, the action and the answer share one band because they share one subject;
/// the band is drawn before the stories so the action sits directly under the name it is
/// about. On a press the window queues one `monitor_run` request and says what happened in
/// one line — `asked the agent to run this monitor now`, or the queue's own sentence when it
/// could not. Nothing is read again: the run happens in another process, and this page's
/// data stays exactly as fresh as it was one click ago.
fn asks_and_run(ui: &mut egui::Ui, width: f32, monitor: &Monitor, queue: &std::path::Path) {
    let line_width = table::detail_width(width);
    let mut pressed = false;
    table::detail_with(ui, width, |ui| {
        ui.set_min_width(line_width);
        ui.set_max_width(line_width);
        ui.add(egui::Label::new(theme::dim(&asks(monitor))).wrap());
        ui.add_space(theme::space::XS);
        pressed = table::request_link(ui, RUN_NOW, RUN_HOVER);
        if let Some(said) = table::said_about(ui.ctx(), PAGE, monitor.id) {
            ui.add_space(theme::space::XS);
            table::said_line(ui, &said);
        }
        // Under the action, not instead of it: what the row can do, then what it cannot.
        ui.add_space(theme::space::XS);
        ui.add(egui::Label::new(theme::dim(NO_DELETE)).wrap());
    });
    if !pressed {
        return;
    }
    let payload = crate::requests::monitor_run_payload(monitor.id, "button");
    let said = match crate::requests::push_to(queue, "monitor_run", &payload) {
        Ok(_) => match monitor.name.trim() {
            "" => "asked the agent to run this monitor now".to_string(),
            name => format!(
                "asked the agent to run {} now",
                super::clip(name, 60)
            ),
        },
        Err(problem) => problem,
    };
    table::remember_said(ui.ctx(), PAGE, monitor.id, &said);
    ui.ctx().request_repaint();
}

/// The page header's action: the reader's own words become a monitor.
///
/// The field's text lives in the context's memory rather than in a `&mut` the shell holds,
/// for the same reason the open row does — the page stays a function of its data. It is a
/// `TextEdit`, which is the one control in the shell that takes typing; it is styled by the
/// theme like every other widget, so it gets the theme's corner, hairline and focus ring.
///
/// Two ways to submit, because a reader will try both: Enter inside the field, or the link
/// beside it. Both go through [`queue_a_monitor`].
fn add_a_monitor(ui: &mut egui::Ui, queue: &std::path::Path) {
    let slot = egui::Id::unique(("atlas-monitor-draft", PAGE));
    let mut draft = ui.data(|data| data.get_temp::<String>(slot)).unwrap_or_default();
    let mut submit = false;
    ui.horizontal(|ui| {
        ui.label(theme::caption(ADD_LABEL));
        let field = ui.add(
            egui::TextEdit::singleline(&mut draft)
                .hint_text(ADD_HINT)
                .desired_width(theme::space::COLUMN * 4.0),
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
            // One field, two keys: the words are the monitor's name and its question.
            let name = super::clip(&typed, NAME_MAX);
            let query = super::clip(&typed, QUERY_MAX);
            let payload = crate::requests::topic_add_payload(&name, &query);
            let said = match crate::requests::push_to(queue, "topic_add", &payload) {
                Ok(_) => {
                    // A sent request clears the field, so the reader can ask for the next
                    // one; a refused one keeps the words, so a failure costs a correction
                    // rather than retyping the sentence.
                    draft.clear();
                    format!("asked the agent to add a monitor for {name}")
                }
                Err(problem) => problem,
            };
            table::remember_said(ui.ctx(), PAGE, HEADER, &said);
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|data| data.insert_temp(slot, draft));
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }
}

/// What the monitor collected, and the state it is in, under its open row.
///
/// Returns the story a reader asked to open, or `None`. Nothing is drawn when the shell
/// read nothing for this monitor (`MonitorStories::none()`, or stories for a different
/// monitor): the page cannot say "no stories" about a list it was never given.
fn stories_under(
    ui: &mut egui::Ui,
    width: f32,
    monitor: &Monitor,
    stories: MonitorStories<'_>,
) -> Option<i64> {
    let rows = stories.under(monitor.id)?;
    let mut asked = None;
    table::detail_with(ui, width, |ui| {
        // The state, the count and the last check: the three things the row can only
        // abbreviate, in one sentence a reader can act on.
        ui.add(egui::Label::new(theme::dim(&state_line(monitor))).wrap());
        ui.add_space(theme::space::XS);

        if rows.is_empty() {
            // Which sentence is true is a fact the archive told us: `hits` counts every
            // story the monitor raised an alert for, the rows came back inside the window.
            let sentence = if monitor.hits <= 0 {
                NOTHING_YET
            } else {
                NONE_IN_WINDOW
            };
            ui.add(egui::Label::new(theme::dim(sentence)).wrap());
            return;
        }
        for story in rows {
            let (response, link) = story_row(ui, story, monitor.id);
            // A press on the row's source link is the link's: the row suppresses its own
            // click while the pointer is over that cell, so one press opens one thing.
            if table::activate(ui, &response) && !crate::views::reader::inside(ui, link) {
                asked = Some(story.id);
            }
        }
    });
    asked
}

/// A story row's identity: stable across frames, and unique per monitor so two monitors'
/// lists cannot share a focus ring — and so a row cannot collide with the monitor row of
/// the same number, which is what a bare `story.id` would do.
pub fn story_row_id(monitor: i64, story: i64) -> egui::Id {
    egui::Id::unique(("atlas-monitor-story", monitor, story))
}

/// One story of an open monitor: title, outlet, day — and a click or Enter that reports
/// its id.
///
/// The height, the hairline and the focus ring are the table's, because this row sits
/// under one and reads as part of it; only the columns are its own (a story has no state
/// and no count). `topics.rs` draws the same three facts with the same widths for its own
/// story list — the two are separate functions today because the id namespace differs and
/// `views::topics` owns its own; if `table` ever grows a story-row primitive, both should
/// call it.
fn story_row(ui: &mut egui::Ui, story: &Article, monitor: i64) -> (egui::Response, egui::Rect) {
    let width = ui.available_width().max(theme::space::COLUMN * 2.0);
    let (_auto, rect) = ui.allocate_space(egui::vec2(width, table::ROW_H));
    let response = ui
        .interact(rect, story_row_id(monitor, story.id), egui::Sense::click())
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
    // The Reader's source link gets a column of its own, measured from the caption it is
    // drawn in rather than typed: the row is a door to the reading pane, and this is the door
    // to the page itself. Its cell is the row's own height, because egui moves a vertical
    // layout's cursor to the bottom of a widget placed with `Ui::put` — a shorter cell would
    // pull the next row up over this one.
    let link_w = crate::views::reader::source_link_width(ui);
    let link = egui::Rangef::new(
        (source.min - STORY_GAP - link_w).max(rect.left()),
        source.min - STORY_GAP,
    );
    let title = egui::Rangef::new(rect.left(), (link.min - STORY_GAP).max(rect.left()));

    table::text(
        ui,
        title,
        y,
        &story_title(story),
        table::body_font(),
        theme::INK,
        egui::Align2::LEFT_CENTER,
    );
    let outlet = one_line(story.source.trim());
    if !outlet.is_empty() {
        table::text(
            ui,
            source,
            y,
            &outlet,
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
            &one_line(&when(&story.published)),
            table::caption_font(),
            theme::DIM,
            egui::Align2::RIGHT_CENTER,
        );
    }
    // The link is drawn last, over the row's own interaction, and it is a widget rather than
    // paint: it takes a press and a keyboard focus of its own. Its cell is the row's full
    // height, so the vertical cursor `Ui::put` moves lands exactly where the row's own
    // allocation left it.
    let link_rect = egui::Rect::from_min_max(
        egui::Pos2::new(link.min, rect.top()),
        egui::Pos2::new(link.max, rect.bottom()),
    );
    let _pressed = crate::views::reader::source_link_in(ui, &story.url, link_rect);
    if response.has_focus() {
        ui.painter()
            .rect_stroke(rect, theme::RADIUS, theme::focus_stroke(), egui::StrokeKind::Inside);
    }
    (response, link_rect)
}

/// A story's row name. The archive can hold a story without a title; a row still has to
/// be clickable and say what it is, and it may not borrow the empty state's opening words.
fn story_title(story: &Article) -> String {
    if story.title.trim().is_empty() {
        "Untitled story".to_string()
    } else {
        one_line(&clip(&story.title, NAME_MAX))
    }
}

/// The state, the alerts it raised and the last time it did anything — one sentence.
///
/// The row has these three facts in three columns and can only abbreviate them: `hits` is
/// a number with no unit, `last activity` is a date with no verb. This is the same answer
/// in words, and it is where a paused monitor is spelled out, because "paused" in a
/// column between a dot and a count is easy to read past and it is the one state that
/// changes what the reader should expect.
fn state_line(monitor: &Monitor) -> String {
    let alerts = match monitor.hits {
        1 => "1 alert".to_string(),
        count => format!("{} alerts", nf(count)),
    };
    let last = if monitor.last_run.trim().is_empty() {
        "it has not run yet".to_string()
    } else {
        format!("it last ran {}", when(&monitor.last_run))
    };
    if monitor.active {
        format!("It is {WATCHING}, has raised {alerts}, and {last}.")
    } else {
        format!(
            "It is {PAUSED}: it asks its question again only when you ask it. It has raised \
             {alerts}, and {last}."
        )
    }
}

/// A value as one line of text a row can hold.
///
/// Two jobs, both about strings this window did not write. Control characters become
/// spaces — a `\n` still starts a second line inside `LayoutJob::simple_singleline`, and a
/// second line in a `ROW_H` row paints over its neighbour; a NUL is not a glyph at all.
/// Runs of whitespace collapse, and the result is cut with [`clip`], which cuts on a
/// character boundary. The same rule `views::when` applies to a stamp, for the same
/// reason, and it is applied to the *drawing* form only: [`row_name`] stays the archive's
/// own bytes, because that is what other tests read.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(NAME_MAX * 2));
    let mut space = false;
    for ch in text.chars() {
        if ch.is_control() || ch.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    clip(&out, NAME_MAX)
}

/// One row's four facts, already worded for the columns.
fn line(monitor: &Monitor) -> table::Line {
    table::Line {
        // The drawing form, not the archive's bytes: `row_name` stays verbatim (a test
        // pins that), and a cell is one line of a fixed height, so a name with a newline
        // in it would paint a second line over the next row.
        name: one_line(&row_name(monitor)),
        state: if monitor.active { WATCHING } else { PAUSED },
        on: monitor.active,
        hits: nf(monitor.hits),
        last: if monitor.last_run.trim().is_empty() {
            NEVER.to_string()
        } else {
            when(&monitor.last_run)
        },
    }
}

/// The name a row shows: the archive's own, or the monitor's id when the archive stored
/// nothing but whitespace. A blank cell is a row a reader cannot name, and the id is the
/// one handle this window has on a monitor it cannot see the name of.
pub fn row_name(monitor: &Monitor) -> String {
    if monitor.name.trim().is_empty() {
        format!("monitor {}", monitor.id)
    } else {
        monitor.name.clone()
    }
}

/// What the row cannot say: the question, and how often it is asked again.
fn asks(monitor: &Monitor) -> String {
    // The query is prose in a wrapping block, so a newline is a line break like any
    // other — but its length is the archive's, not ours, and a megabyte-long query would
    // be a megabyte-long galley laid out on every frame. Bounded, and on a char boundary.
    let query = clip(monitor.query.trim(), QUERY_MAX);
    let cadence = cadence_text(monitor.cadence);
    if query.is_empty() {
        format!("The query is empty, so this monitor cannot match anything. It is {cadence}.")
    } else {
        format!("Asks: {query}\nIt is {cadence}.")
    }
}

/// `cadence_minutes` in words. `0` is not "every 0 minutes": `atlas-store`'s
/// `set_cadence` documents zero as manual only — a monitor the reader asks, not a timer.
///
/// The number comes out of a database this window does not write, so the top of the range
/// has to read as a sentence too: a cadence of `i64::MAX` minutes used to be spelled
/// "asked again every 9223372036854775807 minutes", which is a number nobody can wait
/// for and a sentence nobody can use. Past a year the sentence says so instead of
/// counting — the arithmetic below never multiplies, so no value can overflow it.
pub fn cadence_text(minutes: i64) -> String {
    if minutes <= 0 {
        return "asked only when you ask it".to_string();
    }
    if minutes > A_YEAR {
        return "asked again on a cadence longer than a year".to_string();
    }
    if minutes == 60 {
        return "asked again every hour".to_string();
    }
    if minutes % 60 == 0 {
        // Grouped like every other count in the window (`nf`): `8 760 hours` is read, not
        // decoded. Below 1 000 the grouping is a no-op, so the common rows are unchanged.
        return format!("asked again every {} hours", nf(minutes / 60));
    }
    format!("asked again every {minutes} minutes")
}

/// Where the arrow keys land, clamped to the list: the first row is a stop, not a wrap.
fn landed(index: usize, step: i32, len: usize) -> usize {
    if step > 0 {
        (index + 1).min(len.saturating_sub(1))
    } else {
        index.saturating_sub(step.unsigned_abs() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: i64, name: &str, query: &str, active: bool, cadence: i64) -> Monitor {
        Monitor {
            id,
            name: name.to_string(),
            query: query.to_string(),
            active,
            cadence,
            last_run: String::new(),
            hits: 0,
        }
    }

    /// A cadence of zero is manual, not a loop of no length.
    #[test]
    fn a_cadence_reads_as_a_sentence_and_zero_is_manual() {
        assert_eq!(cadence_text(0), "asked only when you ask it");
        assert_eq!(cadence_text(-5), "asked only when you ask it", "a negative is not a loop");
        assert_eq!(cadence_text(60), "asked again every hour");
        assert_eq!(cadence_text(240), "asked again every 4 hours");
        assert_eq!(cadence_text(90), "asked again every 90 minutes");
    }

    /// The detail carries what the columns cannot: the question and the cadence.
    #[test]
    fn an_open_row_says_the_question_and_the_cadence() {
        let asking = monitor(1, "Chip export rules", "chip export restrictions", true, 120);
        let detail = asks(&asking);
        assert!(detail.contains("chip export restrictions"), "{detail}");
        assert!(detail.contains("asked again every 2 hours"), "{detail}");
        // a monitor whose query was never written says so instead of asking nothing
        let empty = monitor(2, "Unwritten", "   ", true, 0);
        let detail = asks(&empty);
        assert!(detail.contains("The query is empty"), "{detail}");
        assert!(detail.contains("asked only when you ask it"), "{detail}");
        // the page may not paint the opening words of its own empty state
        assert!(!detail.contains("No "), "{detail}");
    }

    /// The row: the state is one word with a dot, and a check that never ran says so.
    #[test]
    fn a_row_reports_the_state_and_the_last_check() {
        let running = monitor(1, "Chips", "q", true, 60);
        let row = line(&running);
        assert_eq!((row.name.as_str(), row.state, row.on), ("Chips", "watching", true));
        assert_eq!(row.last, NEVER);
        assert_eq!(row.hits, "0");
        let paused = monitor(2, "", "q", false, 60);
        let row = line(&paused);
        assert_eq!((row.name.as_str(), row.state, row.on), ("monitor 2", "paused", false));
    }

    /// One frame of the page with no window in sight: every string it painted. Two
    /// frames, because the first lays the table out and the second paints it where it
    /// landed. The columns themselves are proved once, in `views::topics` — this is here
    /// so a row of *this* page cannot drift from them without a test noticing.
    fn painted(context: &egui::Context, monitors: &[Monitor]) -> Vec<String> {
        let screen = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let mut out = Vec::new();
        for _ in 0..2 {
            let output = context.run_ui(screen.clone(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| show(ui, monitors));
            });
            out.clear();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut out);
            }
            // egui 0.37 asserts in debug when a `FullOutput` is dropped with texture
            // deltas still in it; a headless frame has nothing to upload them to.
            output.drop_without_applying_deltas();
        }
        out
    }

    fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Vec(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
            _ => {}
        }
    }

    /// The table, drawn: a header that names the four columns, and one row per monitor.
    #[test]
    fn the_page_paints_a_header_and_its_rows() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let monitors = vec![
            monitor(1, "Chip export rules", "chip export restrictions", true, 60),
            monitor(2, "North Sea ferries", "ferry north sea traffic", false, 240),
        ];
        let painted = painted(&context, &monitors);
        for painted_text in ["name", "state", "hits", "last activity", "Chip export rules", "North Sea ferries", "watching", "paused", NEVER] {
            assert!(painted.iter().any(|text| text == painted_text), "{painted_text:?} was not painted");
        }
        // the same contract Topics keeps: a page with rows never says the archive is empty
        assert!(
            painted.iter().all(|text| !text.starts_with("No ")),
            "a full page painted its empty state"
        );
    }

    /// An open row says the question and how often it is asked again.
    #[test]
    fn an_open_row_paints_the_question_and_the_cadence() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let monitors = vec![monitor(1, "Chip export rules", "chip export restrictions", true, 120)];

        let shut = painted(&context, &monitors);
        assert!(shut.iter().any(|text| text == HINT), "the page does not say what a click does");

        table::remember(&context, PAGE, Some(1));
        let open = painted(&context, &monitors);
        let detail = open
            .iter()
            .find(|text| text.starts_with("Asks: "))
            .unwrap_or_else(|| panic!("an open row painted no question; it painted: {open:?}"));
        assert!(detail.contains("chip export restrictions"), "{detail}");
        assert!(detail.contains("asked again every 2 hours"), "{detail}");
        assert!(!open.iter().any(|text| text == HINT), "the hint survived the detail");
    }

    /// The empty state is a sentence that names the page and the next step.
    #[test]
    fn the_empty_state_is_a_sentence_that_says_what_to_do() {
        assert!(EMPTY.starts_with("No monitor yet"), "{EMPTY}");
        assert!(EMPTY.ends_with('.'), "{EMPTY}");
        assert!(EMPTY.len() > 24, "{EMPTY}");
        assert!(EMPTY.contains("atlas-monitor add"), "the next step is named: {EMPTY}");
    }

    /// The arrow keys stop at the ends: a table does not wrap, or a reader pressing Down
    /// on the last row would be sent back to the first.
    #[test]
    fn the_keyboard_stops_at_the_last_row() {
        assert_eq!(landed(0, 1, 3), 1);
        assert_eq!(landed(2, 1, 3), 2, "the last row is a stop");
        assert_eq!(landed(0, -1, 3), 0, "the first row is a stop");
        assert_eq!(landed(1, -1, 3), 0);
        assert_eq!(landed(0, 1, 0), 0, "an empty list has nowhere to go");
    }

    /// A row's cell is one line, and the archive's own bytes are still what `row_name`
    /// answers: the flattening is the *drawing* form only, so a test that reads the name
    /// keeps seeing what was stored.
    #[test]
    fn a_row_draws_a_hostile_name_on_one_line_without_changing_it() {
        let hostile = format!("{}\n{}\0{}", "A".repeat(3_000), "second line", "nul");
        let raw = monitor(3, &hostile, "q", true, 1);
        assert_eq!(row_name(&raw), hostile, "`row_name` is the archive's own bytes");

        let cell = line(&raw).name;
        assert!(!cell.chars().any(char::is_control), "{cell:?}");
        assert!(cell.chars().count() <= NAME_MAX + 1, "{} characters", cell.chars().count());
        assert!(cell.starts_with("AAAA"), "{cell:?}");
        assert!(!cell.contains("second line"), "the line after the break is gone: {cell:?}");
    }

    /// The detail says the three things the columns abbreviate: the state, the alerts and
    /// the last check — and it spells "paused" out rather than relying on one column.
    #[test]
    fn the_state_line_reads_the_state_the_hits_and_the_last_run() {
        let watching = Monitor {
            hits: 12,
            last_run: "2026-11-22T10:00:00Z".to_string(),
            ..monitor(1, "Harbour labour", "q", true, 12)
        };
        let sentence = state_line(&watching);
        assert!(sentence.contains("It is watching"), "{sentence}");
        assert!(sentence.contains("12 alerts"), "{sentence}");
        assert!(sentence.contains("it last ran"), "{sentence}");

        let never = monitor(2, "Unasked", "q", true, 0);
        let sentence = state_line(&never);
        assert!(sentence.contains("0 alerts"), "{sentence}");
        assert!(sentence.contains("has not run yet"), "{sentence}");

        let one = Monitor {
            hits: 1,
            ..monitor(3, "One alert", "q", true, 1)
        };
        assert!(state_line(&one).contains("1 alert,"), "{}", state_line(&one));

        let paused = monitor(4, "Chip export rules", "q", false, 5);
        let sentence = state_line(&paused);
        assert!(sentence.contains("It is paused"), "{sentence}");
        assert!(sentence.contains("only when you ask it"), "{sentence}");
    }

    // ------------------------------------------------------------------ the request links

    /// What one frame painted: every string, and where it sits.
    #[derive(Default)]
    struct Shown {
        text: Vec<(String, egui::Rect)>,
    }

    impl Shown {
        fn any(&self, value: &str) -> bool {
            self.text.iter().any(|(text, _)| text == value)
        }

        fn at(&self, value: &str) -> egui::Rect {
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

        fn starting(&self, start: &str) -> Option<&str> {
            self.text
                .iter()
                .find(|(text, _)| text.starts_with(start))
                .map(|(text, _)| text.as_str())
        }
    }

    /// A queue of the test's own, in a directory it deletes: the links write a real row, and
    /// never into the checkout's `data/`.
    fn queue() -> (std::path::PathBuf, std::path::PathBuf) {
        let name = format!(
            "atlas-monitors-queue-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(name.replace(['(', ')', ' '], "-"));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("data").join("requests.db");
        (dir, file)
    }

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

    /// One frame of the page, with `events` delivered to it and no story list — the page as
    /// `app.rs` draws it before it has read the open row's stories.
    fn frame(
        context: &egui::Context,
        monitors: &[Monitor],
        queue: &std::path::Path,
        events: Vec<egui::Event>,
    ) -> Shown {
        frame_showing(context, monitors, MonitorStories::none(), queue, events)
    }

    /// [`frame`] with the stories the shell read for one monitor.
    fn frame_showing(
        context: &egui::Context,
        monitors: &[Monitor],
        stories: MonitorStories<'_>,
        queue: &std::path::Path,
        events: Vec<egui::Event>,
    ) -> Shown {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        input.events = events;
        let mut out = Shown::default();
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .show(ui, |ui| show_with_queue(ui, monitors, stories, queue));
        });
        fn collect(shape: &egui::Shape, out: &mut Shown) {
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
                _ => {}
            }
        }
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        output.drop_without_applying_deltas();
        out
    }

    /// A click on `at`: the press frame and the release frame a real pointer sends.
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

    /// The action under an open monitor queues one `monitor_run` request for that monitor and
    /// says what it did — one line, no spinner, no second read.
    #[test]
    fn the_open_monitor_queues_one_run_and_says_so_in_one_line() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let monitors = vec![monitor(3, "Chip export rules", "chip export", true, 120)];
        remember_open(&context, Some(3));

        let first = frame(&context, &monitors, &queue, Vec::new());
        let at = first.at(RUN_NOW).center();
        frame(&context, &monitors, &queue, click(at));
        let after = frame(&context, &monitors, &queue, Vec::new());

        let said = after
            .starting("asked the agent to run")
            .unwrap_or_else(|| {
                panic!(
                    "the page did not say what it did: {:?}",
                    after.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
                )
            })
            .to_string();
        assert_eq!(said, "asked the agent to run Chip export rules now", "{said}");
        // Queued, never "done": the run happens in another process, so a page claiming it
        // finished would be claiming a read it did not make.
        for claim in ["ran ", "has run", "finished", "done", "collected", "complete"] {
            assert!(
                !said.to_lowercase().contains(claim),
                "the page claims the run is over (`{claim}`): {said}"
            );
        }
        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press queued {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "monitor_run");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("valid JSON");
        assert_eq!(payload["id"], 3);
        assert_eq!(payload["reason"], "button");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The header's action: what the reader typed becomes a `topic_add` request, and the
    /// field is cleared for the next one. Typed here the way a reader types — a click into
    /// the field and then the text — so the wiring tested is the one that ships.
    #[test]
    fn the_header_queues_what_the_reader_typed_as_a_monitor() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let monitors = vec![monitor(1, "Harbour labour", "harbour", true, 60)];

        let first = frame(&context, &monitors, &queue, Vec::new());
        assert!(first.any(ADD_LABEL), "the header paints no `{ADD_LABEL}`");
        // Click the field (the hint is what it paints when empty), then type into it.
        let field = first.at(ADD_HINT).center();
        frame(&context, &monitors, &queue, click(field));
        let typed = frame(
            &context,
            &monitors,
            &queue,
            vec![egui::Event::Text("what is happening with chips?".to_string())],
        );
        // What the field *holds* is read from the page's own slot rather than from the
        // frame's shapes: egui paints a text edit's galley from the state the frame started
        // with, so the typed characters appear one frame after the event. The state is the
        // thing this page owns and the thing the submit will read, so it is the thing to
        // assert on — the painted line is asserted at the end, where the field is cleared.
        let draft = context
            .data(|data| data.get_temp::<String>(egui::Id::unique(("atlas-monitor-draft", PAGE))))
            .unwrap_or_default();
        assert_eq!(
            draft, "what is happening with chips?",
            "the field did not take the typed text"
        );
        // And the link submits it.
        let at = typed.at(ADD_ACTION).center();
        frame(&context, &monitors, &queue, click(at));
        let after = frame(&context, &monitors, &queue, Vec::new());

        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press queued {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "topic_add");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("valid JSON");
        assert_eq!(payload["query"], "what is happening with chips?");
        assert_eq!(
            payload["name"], "what is happening with chips?",
            "one field is both name and question"
        );
        assert!(
            after
                .starting("asked the agent to add a monitor for")
                .is_some(),
            "the page did not say what it did: {:?}",
            after.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        // Cleared, not left behind: the reader's next sentence starts in an empty field.
        assert!(
            !after.any("what is happening with chips?"),
            "the field kept the words that were already sent"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Pressing the header's action with nothing typed is a sentence, not a row: the queue
    /// refuses an empty payload, and the page says why before the queue has to.
    #[test]
    fn an_empty_field_queues_nothing_and_says_what_to_do() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let monitors = vec![monitor(1, "Harbour labour", "harbour", true, 60)];

        let first = frame(&context, &monitors, &queue, Vec::new());
        let at = first.at(ADD_ACTION).center();
        frame(&context, &monitors, &queue, click(at));
        let after = frame(&context, &monitors, &queue, Vec::new());

        assert!(after.any(ADD_EMPTY), "the page painted no sentence: {:?}", after.text);
        assert!(queued(&queue).is_empty(), "an empty field wrote a row");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A monitor's story row is two doors: the row opens the reading pane, the "Read the
    /// source" cell opens the Reader. The cells overlap on purpose, so a press must land on
    /// exactly one of them — the composed answer below is the one `stories_under` acts on.
    #[test]
    fn a_monitor_story_row_opens_the_story_or_its_source_but_never_both() {
        use crate::views::reader::harness;
        let story = Article {
            id: 21,
            title: "Ferry traffic resumes".to_string(),
            source: "Harbour Post".to_string(),
            published: "2024-05-01T12:54:00Z".to_string(),
            url: "https://harbour.example/news/ferry".to_string(),
            ..Article::default()
        };
        // The row as the page reads it: the row's own activation, suppressed while the
        // pointer is over the source link's cell.
        let opens = |ui: &mut egui::Ui| {
            let (response, link) = story_row(ui, &story, 4);
            table::activate(ui, &response) && !crate::views::reader::inside(ui, link)
        };

        let ctx = harness::fresh();
        // Two frames: the row measures its columns on the frame it draws them.
        let _ = harness::drive(&ctx, Vec::new(), |ui| {
            let _ = opens(ui);
        });
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| {
            let _ = opens(ui);
        });
        assert!(
            painted.has("Read the source"),
            "the row does not offer the page:\n{}",
            painted.text
        );

        // The link's door: the address leaves for the shell, and the row does not open.
        let at = painted.at("Read the source");
        let (answers, _) = harness::press(&ctx, at, opens);
        assert_eq!(
            crate::views::reader::take_source(&ctx).as_deref(),
            Some("https://harbour.example/news/ferry"),
            "a press on the row's source link did not hand the shell the row's address"
        );
        assert!(
            !answers.iter().any(|opened| *opened),
            "a press on the source link also opened the story"
        );

        // The row's own door: the story opens, and nothing was handed to the Reader.
        let title = painted.at("Ferry traffic resumes");
        let (answers, _) = harness::press(&ctx, title, opens);
        assert!(
            answers.iter().any(|opened| *opened),
            "a press on the row did not open the story it names"
        );
        assert_eq!(
            crate::views::reader::take_source(&ctx),
            None,
            "a press on the row also opened the Reader"
        );
    }

    // ------------------------------------------------- the page's six behaviours, pinned

    /// One story with the words it has and nothing else.
    fn story(id: i64, title: &str) -> Article {
        Article {
            id,
            title: title.to_string(),
            source: "Harbour Post".to_string(),
            published: "2024-05-01T12:54:00Z".to_string(),
            ..Article::default()
        }
    }

    /// (1) A monitor row is a door: a press opens *that* row's detail, a second press closes
    /// it. The story rows under it and the two request links have tests of their own; this
    /// is the row a reader presses first, and the failure it guards is opening the wrong
    /// monitor — one row's question drawn under another row's name.
    #[test]
    fn a_monitor_row_opens_its_own_detail_on_a_click() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let monitors = vec![
            monitor(1, "Chip export rules", "chip export restrictions", true, 60),
            monitor(2, "North Sea ferries", "ferry north sea traffic", false, 240),
        ];

        let first = frame(&context, &monitors, &queue, Vec::new());
        assert_eq!(open_monitor(&context), None, "a page opened with a row already open");
        assert!(first.any(HINT), "a shut page does not offer the row");

        // The second row, by its own name: the detail that appears must be its question.
        let at = first.at("North Sea ferries").center();
        frame(&context, &monitors, &queue, click(at));
        let open = frame(&context, &monitors, &queue, Vec::new());
        assert_eq!(open_monitor(&context), Some(2), "the press opened the wrong row");
        let question = open
            .starting("Asks: ")
            .unwrap_or_else(|| panic!("no question under the open row: {:?}", open.text));
        assert!(
            question.contains("ferry north sea traffic"),
            "the detail under `North Sea ferries` is another monitor's: {question}"
        );
        assert!(
            !question.contains("chip export restrictions"),
            "the detail carries the other row's question: {question}"
        );
        assert!(!open.any(HINT), "the hint survived an open row");

        // The same row, pressed again, is the way out.
        let at = open.at("North Sea ferries").center();
        frame(&context, &monitors, &queue, click(at));
        let shut = frame(&context, &monitors, &queue, Vec::new());
        assert_eq!(open_monitor(&context), None, "a second press did not close the row");
        assert!(shut.any(HINT), "the closed page does not say what a click does");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (4) The one thing the row cannot do, said plainly. The window's archive is read-only
    /// and the queue has no deletion kind, so the page neither offers a dead button nor
    /// stays silent about it: the sentence names the tool that *can* delete a monitor.
    #[test]
    fn an_open_monitor_says_plainly_that_it_cannot_be_deleted() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let monitors = vec![monitor(3, "Chip export rules", "chip export", true, 60)];

        // Nothing on a shut page: a caveat belongs to the row it is about.
        let shut = frame(&context, &monitors, &queue, Vec::new());
        assert!(
            !shut.any(NO_DELETE),
            "a shut page explains a control it has not shown"
        );

        remember_open(&context, Some(3));
        let open = frame(&context, &monitors, &queue, Vec::new());
        assert!(
            open.any(NO_DELETE),
            "the open row does not say it cannot be deleted: {:?}",
            open.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        // The sentence is the interface's voice: it says what is not possible, and where the
        // reader can do it instead.
        assert!(NO_DELETE.contains("cannot delete"), "{NO_DELETE}");
        assert!(NO_DELETE.contains("atlas-monitor delete"), "{NO_DELETE}");
        assert!(NO_DELETE.ends_with('.'), "{NO_DELETE}");
        assert!(
            !NO_DELETE
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0].is_ascii_uppercase() && pair[1].is_ascii_uppercase()),
            "the sentence shouts: {NO_DELETE}"
        );
        // Drawing the caveat asks for nothing: a deletion the window cannot perform must not
        // become a row another process has to refuse.
        assert!(
            queued(&queue).is_empty(),
            "the page queued a request just by saying it cannot delete"
        );
        // And it is measured with the row's other prose, not squeezed to one line: the band
        // wraps it like every other sentence on the page.
        let at = open.at(NO_DELETE);
        assert!(at.width() > 0.0 && at.height() > 0.0, "{at:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (5) An open monitor with nothing under it is a sentence, and *which* sentence is a
    /// fact about the monitor: `hits` counts everything it ever raised, the rows are the
    /// window the shell read. A list the shell never read is neither of those, so the page
    /// claims nothing at all — silence, not "no stories".
    #[test]
    fn a_monitor_with_no_stories_says_so_in_a_sentence() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let never = vec![monitor(4, "Chip export rules", "chip export", true, 60)];
        remember_open(&context, Some(4));

        let empty = frame_showing(&context, &never, MonitorStories::of(4, &[]), &queue, Vec::new());
        assert!(
            empty.any(NOTHING_YET),
            "a monitor that collected nothing paints no sentence: {:?}",
            empty.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );
        assert!(
            !empty.any(NONE_IN_WINDOW),
            "a monitor that never collected says its stories are out of the window"
        );

        // The same monitor, later: nine alerts, none of them inside the window read.
        let collected = vec![Monitor {
            hits: 9,
            ..monitor(4, "Chip export rules", "chip export", true, 60)
        }];
        let outside = frame_showing(
            &context,
            &collected,
            MonitorStories::of(4, &[]),
            &queue,
            Vec::new(),
        );
        assert!(outside.any(NONE_IN_WINDOW), "the window's own sentence is missing");
        assert!(!outside.any(NOTHING_YET), "nine alerts, called `nothing yet`");

        // Nothing was read for this row: the page was never told, so it says nothing.
        let unread = frame(&context, &collected, &queue, Vec::new());
        assert!(
            !unread.any(NOTHING_YET) && !unread.any(NONE_IN_WINDOW),
            "the page invented an empty state for a list it was never handed: {:?}",
            unread.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
        );

        // And with stories in hand the list is drawn instead of any sentence.
        let rows = [story(11, "Ferry traffic resumes"), story(12, "Crane strike ends")];
        let listed = frame_showing(&context, &collected, MonitorStories::of(4, &rows), &queue, Vec::new());
        assert!(listed.any("Ferry traffic resumes"), "the first story was not drawn");
        assert!(listed.any("Crane strike ends"), "the second story was not drawn");
        assert!(!listed.any(NOTHING_YET) && !listed.any(NONE_IN_WINDOW));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (6) No word list decides what this page shows.
    ///
    /// The judgement about what a monitor collected belongs to the model — that is the whole
    /// reason the page is a table and not a filter — so the page draws every story the shell
    /// hands it and never compares one to a list of words. Proved twice: by behaviour (a
    /// monitor whose question shares no word with its stories draws all of them, which a
    /// `contains`-style filter — the shape `views::articles` uses for its own *search* —
    /// could not do) and by source (the drawing half of this file contains no text
    /// predicate at all). The crate-wide scan is `tests/no_word_lists.rs`.
    #[test]
    fn no_word_list_decides_what_this_page_shows() {
        // -- behaviour
        let context = egui::Context::default();
        crate::theme::install(&context);
        let (dir, queue) = queue();
        let monitor = vec![monitor(6, "Chip export rules", "chip export restrictions", true, 5)];
        remember_open(&context, Some(6));
        // Three stories a blocklist would call "not news", and a question that mentions none
        // of their words: every one of them is still drawn, with its own title.
        let rows = [
            story(31, "Horoscope for Tuesday"),
            story(32, "Weather: rain again"),
            story(33, "Celebrity gossip round-up"),
        ];
        let shown = frame_showing(&context, &monitor, MonitorStories::of(6, &rows), &queue, Vec::new());
        for row in &rows {
            assert!(
                shown.any(&row.title),
                "{:?} was hidden — the page is judging what is news: {:?}",
                row.title,
                shown.text.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>()
            );
        }
        // The question is drawn as the archive stored it, whatever it says.
        let question = shown.starting("Asks: ").expect("no question under the open row");
        assert!(question.contains("chip export restrictions"), "{question}");
        let _ = std::fs::remove_dir_all(dir);

        // -- source shape
        const SOURCE: &str = include_str!("monitors.rs");
        let drawing = SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("this file is drawn before its tests");
        for shape in ["to_lowercase(", ".retain(", ".filter(", ".contains("] {
            assert!(
                !drawing.contains(shape),
                "`views/monitors.rs` contains `{shape}` before its test module: a predicate \
                 over text on this page is a word filter waiting to happen, and the judgement \
                 belongs to the model (see `tests/no_word_lists.rs`)"
            );
        }
    }
}
