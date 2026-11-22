//! Articles: the list on the left, the reading pane on the right.

use super::{heading, nf, when};
// `clip` lives in `views::mod`, because every view clips something; re-exported here so a
// caller that already says `articles::clip` keeps working.
pub use super::clip;
use crate::data::Article;
use crate::theme;

/// How much of the page's width the list takes when the page is wide enough to hold two
/// columns. A little under half keeps the list a list: the reader scans it, then reads.
const LIST_SHARE: f32 = 0.45;

/// The narrowest the list is allowed to get: a title, its source line and its summary at
/// `theme::dim`'s 12 px, without a title having to wrap into the row below it.
const LIST_FLOOR: f32 = 320.0;

/// The widest the list is allowed to get. Measured against the alternative: at 640 px a
/// row of 13 px body text is ~90 characters, which is as far as the eye tracks back to
/// the next row without losing its place; wider, and the list is the page and the story
/// is the margin.
const LIST_CEILING: f32 = 640.0;

/// The narrowest a reading column may be: 260 px holds ~36 characters of the 13 px face.
/// Below that a paragraph is a stack of two-word lines, which is the reported "the story
/// cannot be read" (measured at 900×620 before this: 152 px, ~11 characters a line).
const PANE_FLOOR: f32 = 260.0;

/// The selection bar's width, in pixels: the same mark the graph's rows carry
/// (`views/graph.rs:2505`, `MARK`).
///
/// A selected row is also filled, but the fill is `theme::SELECT` over the page — measured
/// 1.44:1, which is ink alone and nothing else. A shape carries a selection at any contrast, so
/// the row wears the bar the graph's marked row wears, and the text is inset by it on every row
/// so selecting one does not move it.
const SELECTION_MARK: f32 = 2.0;

/// Below this page width the two columns stop being an honest offer, and the page stacks
/// instead: list on top, story underneath.
///
/// Where 820 comes from: the split has to satisfy the floor *below* the share, and both
/// ends of that are arithmetic, not taste. At 45 %, the list reaches its 320 px floor at
/// 320 / 0.45 = 711 px of page, and the pane keeps its own floor while the list takes its
/// share until page − 0.55·page = PANE_FLOOR, i.e. 260 / 0.55 = 473 px. The binding case
/// is therefore the list: from 711 px up, the share and the floor agree, and the pane is
/// still (711 − 320) = 391 px, comfortably over its floor. But a 320 px list beside a
/// 391 px pane in a 700 px page is two columns of nothing much, and the pane is the thing
/// the reader came for — so the stack starts higher, at 820 px: below it the list is given
/// the full width (where titles never wrap) and the story gets every pixel of height
/// instead of a third of them. `SPLIT_FLOOR` is a page width, not a window width: the
/// window also pays the rail (188) and the page margins (40), so 820 px of page is a
/// ~1047 px window — one column becomes the better page well before that.
const SPLIT_FLOOR: f32 = 820.0;

/// In the stacked page the list is given this share of the height that is still free after
/// the heading and the search row, clamped so that the story always keeps a pane.
///
/// The clamp is not decoration: inside the app's page `ScrollArea` the free height can be
/// unbounded (a scrolling page grows downwards), and an unclamped share of infinity is a
/// list that swallows the story.
const STACKED_LIST_SHARE: f32 = 0.45;
const STACKED_LIST_FLOOR: f32 = 120.0;
const STACKED_LIST_CEILING: f32 = 260.0;

/// The least the story under a stacked list may be given: its own header (a title, a meta line
/// and the space between them, measured 68 px) plus the 100 px of body
/// `tests/search_filter.rs` asks for at 700×520. The list yields to this before it yields to
/// anything else.
const STACKED_PANE_FLOOR: f32 = 188.0;

/// The shortest a stacked list may become when the page cannot hold both floors: one row's title
/// line and the air above it, so the list still paints a row a reader can press. Below this the
/// list is not a list — the story is the page.
const STACKED_LIST_MIN: f32 = 24.0;

/// The room a briefing card is assumed to need before the page has measured one: its sentence, a
/// bullet, its footer, its action and the answer a press writes there — 149 px at the page widths
/// in the audit, and 200 as the guess a first frame uses.
///
/// A card is drawn whole or not at all. Measured at 700×520 before this: the heading, the search
/// row and a 137 px card took 249 px of the 481 px page, the split was handed 200 px (168 once
/// the shell's way-back line is on screen), the list took its 120 px floor and the story was left
/// 48 px — no body at all, which is the audit's "the pane painted no story". And a card shown in
/// a box shorter than itself is worse than no card: a scroll area around it swallowed the
/// sentence a Refresh writes (measured — the frame after the press painted the card and not its
/// answer), because the last line added inside a scroll area is below the fold it measured.
///
/// So the card takes what is left once the two columns have their floors, and where that is not
/// enough it is not drawn — the window's own briefing is on the Overview at every size, so a
/// 520 px window loses nothing it cannot reach.
const CARD_WHOLE: f32 = 200.0;

#[derive(Default)]
pub struct ArticlesView {
    pub needle: String,
    pub selected: Option<i64>,
}

/// The search box as a comparison key: trimmed, lower-cased.
///
/// The same normalisation `crate::app::needle_key` uses to decide whether a read is
/// needed, and the same one SQLite's `LIKE` applies to `crate::data`'s predicate, so
/// "the same search" means one thing in all three places.
fn search_key(needle: &str) -> String {
    needle.trim().to_lowercase()
}

/// Does `haystack` hold `key`? `key` is [`search_key`]'s output: already trimmed and
/// lower-cased, so this is a substring test.
///
/// The ASCII path allocates nothing, because a frame draws up to 400 rows and a search
/// is drawn while the reader types — a `to_lowercase()` per column per row per frame is
/// a copy of the whole list every 16 ms. The non-ASCII path folds with Unicode rules,
/// which is *more* permissive than SQLite's ASCII-only `LIKE`: it can show a row the
/// archive's query also matched (never hide one), which is the safe direction for a
/// display filter — see [`show`].
fn holds(haystack: &str, key: &str) -> bool {
    if key.is_empty() {
        return true;
    }
    if haystack.is_ascii() {
        let hay = haystack.as_bytes();
        let needle = key.as_bytes();
        return hay.len() >= needle.len()
            && hay
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.to_lowercase().contains(key)
}

/// Does this story hold the key in one of the three columns the list can show a hit in?
/// The same three `crate::data::ARTICLE_SEARCH` searches, so a row the archive's query
/// returned is never hidden here.
fn a_hit(story: &Article, key: &str) -> bool {
    holds(&story.title, key) || holds(&story.summary, key) || holds(&story.content, key)
}

/// Draw the list. Returns the id whose reading pane is open.
///
/// The list is narrowed **here**, by the view's own needle, and not only by the query
/// the caller ran. Two reasons, both measured (`tests/search_filter.rs`):
///
/// * The rows handed in are whatever the caller last read, so while the reader types they
///   belong to the previous needle — a view that trusts them draws the new needle above
///   the old rows.
/// * `crate::app::Atlas::reload` appends the newest story at every pin to the list it
///   just read, because a dot on the map has to open something. Those rows are not
///   filtered by the needle: measured on a four-story fixture, a search for "chip" —
///   one match — painted **three** rows, the match and one story per place. The list is
///   capped at 400 rows and the top-up may read 600 more by id, so on the shipped
///   archive the search's own answer can be one row among a thousand things the reader
///   did not search for, newest first. That is the reported "when I choose a filter,
///   nothing comes": nothing they searched for is where they are looking.
///
/// So the view decides what a search shows, from the rows it was given, and the query
/// stays what it is for: not reading the whole archive to draw one screen. A narrowed
/// list therefore cannot lie about its own count (the heading counts the rows drawn),
/// and the reading pane can tell "the search hides it" from "the archive lost it".
pub fn show(ui: &mut egui::Ui, view: &mut ArticlesView, articles: &[Article], hours: i64) -> Option<i64> {
    show_briefed(ui, view, articles, hours, None, None)
}

/// The list with the **briefing card** above it.
///
/// `ask` is the briefing this page's scope has (`crate::views::brief::Ask` — the chip, topic or
/// monitor in force, or the window), and `queue` is the request queue the action writes to. Both
/// are handed in rather than read here: a view that opened the archive or the queue would be a
/// second copy of the shell's read and write boundary, and this file only draws.
///
/// The card sits above the list and above the reading pane — one briefing for the scope the
/// whole page is about, not one per column — and the story a bullet opens comes back as this
/// function's answer, exactly like a row's press. [`show`] is this function with no briefing and
/// no queue, which is what a caller that has neither still calls.
pub fn show_briefed(
    ui: &mut egui::Ui,
    view: &mut ArticlesView,
    articles: &[Article],
    hours: i64,
    ask: Option<&crate::views::brief::Ask<'_>>,
    queue: Option<&std::path::Path>,
) -> Option<i64> {
    let key = search_key(&view.needle);
    let matching = if key.is_empty() {
        articles.len()
    } else {
        articles.iter().filter(|story| a_hit(story, &key)).count()
    };
    let note = if key.is_empty() {
        format!(
            "{} stories in the last {} h, newest first",
            nf(articles.len() as i64),
            hours
        )
    } else {
        // The needle itself is not echoed: a label that reader text can turn into two
        // lines is not a status line.
        format!(
            "{} of {} stories match, in the last {} h, newest first",
            nf(matching as i64),
            nf(articles.len() as i64),
            hours
        )
    };
    heading(ui, "Articles", &note);
    ui.horizontal(|ui| {
        ui.label(theme::dim("Search"));
        let response = ui.add(
            egui::TextEdit::singleline(&mut view.needle)
                .hint_text("a name, a place, a phrase")
                .desired_width(320.0),
        );
        // The edit is drawn for its side effect on `view.needle`, not for its response:
        // the list below is narrowed by the needle in the same frame, so a typed letter
        // never has to wait for a read to take effect (the read follows on the next
        // frame and can only narrow what is already true).
        let _ = response;
        // Only when there is something to clear. An enabled button that has nothing to do is
        // a click the reader pays for and gets nothing back — the same kind of dead target as
        // the dead half of a row. (A box holding only spaces still gets the button: there are
        // characters in it, even though they search for nothing.)
        if !view.needle.is_empty() && ui.button("Clear").clicked() {
            view.needle.clear();
        }
    });
    ui.add_space(10.0);

    // The briefing, above both columns: it answers the question the *page* is about, so it is
    // not repeated in the list and the pane. A bullet that cites a story opens it — the same
    // answer this function returns for a row, so the shell has one door to answer.
    let opened = match ask {
        Some(ask) => {
            // The card's own height, measured on the frame it last drew: a card the page cannot
            // hold whole is not drawn (see [`CARD_WHOLE`]), because a half-shown card hides the
            // action and the sentence the action writes.
            let cost = ui
                .ctx()
                .data(|data| data.get_temp::<f32>(card_cost_id()))
                .unwrap_or(CARD_WHOLE);
            let between = ui.spacing().item_spacing.y + crate::theme::space::MD;
            let need = cost + between + STACKED_LIST_MIN + STACKED_PANE_FLOOR;
            if ui.available_height() < need {
                None
            } else {
                let top = ui.cursor().top();
                let open = crate::views::brief::show(ui, ask, queue);
                let cost = ui.cursor().top() - top;
                if cost.is_finite() && cost > 0.0 {
                    ui.ctx().data_mut(|data| data.insert_temp(card_cost_id(), cost));
                }
                ui.add_space(crate::theme::space::MD);
                open
            }
        }
        None => None,
    };

    // One page, two layouts, chosen by the width the page actually has — never by the
    // window's, because the rail and the page margins have already been paid: measured in
    // `tests/search_filter.rs`, a 700 px window hands this function 596 px and a 1280 px
    // window 1052 px (the rail folds to marks below its own threshold, `src/app.rs`).
    //
    // The split used to be `Panel::left("article-list").default_size(520.0)`, a fixed
    // width. The layout audit measured the result with the rail unfolded: at 700×520 the
    // list was 471 of the 473 px page and **the pane was not painted at all** (no text to
    // the right of x = 528); at 900×620 the pane was 152 px, ~11 characters a line. With
    // today's folded rail the same code measures a 258 px pane at 700×520 and 107 px at
    // 560×480. A width written into the view is a width that is wrong on some screen.
    let page_width = ui.available_width();
    // The story a press opened on *this* frame, in whichever column the page drew: both
    // branches below assign it, and the page answers it.
    let clicked: Option<i64>;
    if page_width < SPLIT_FLOOR {
        // Stacked: the list keeps the whole width (no title wraps) and is bounded in
        // height, so the story below it always has a column to itself.
        //
        // The height is read from the *visible* page (`clip_rect`), not from the free
        // space: inside the app's page `ScrollArea` the free height is unbounded (a
        // scrolling page grows downwards), and a share of infinity is a list that eats the
        // story. The clamp is the second half of the same guard.
        let room = ui.available_height().max(0.0);
        // The story keeps its floor before the list keeps its own: the list yields first, down to
        // one row, because a list over a sliver of a story is the defect the audit found.
        let pane_floor = STACKED_PANE_FLOOR.min((room - STACKED_LIST_MIN).max(0.0));
        let list_height = (room * STACKED_LIST_SHARE)
            .clamp(STACKED_LIST_FLOOR, STACKED_LIST_CEILING)
            .min((room - pane_floor).max(STACKED_LIST_MIN));
        clicked = egui::Panel::top("article-list")
            .resizable(true)
            // The cap is this frame's own height: `Panel` remembers the size the reader left it
            // at, and a remembered 120 px is a story with no column the moment the page shrinks.
            .size_range(egui::Rangef::new(
                STACKED_LIST_MIN,
                list_height.max(STACKED_LIST_MIN),
            ))
            .default_size(list_height)
            .show(ui, |ui| list(ui, view, articles, &key, matching, [false, false]))
            .inner;
        egui::CentralPanel::default().show(ui, |ui| {
            pane(ui, view, articles, &key);
        });
    } else {
        // Two columns: the share, held inside its floor and ceiling. The ceiling also
        // yields to the pane's own floor (`page_width - PANE_FLOOR`), so no drag and no
        // window size can squeeze the reading column under 260 px.
        let list_width = (page_width * LIST_SHARE).clamp(LIST_FLOOR, LIST_CEILING);
        let widest = LIST_CEILING.min(page_width - PANE_FLOOR);
        clicked = egui::Panel::left("article-list")
            .resizable(true)
            .size_range(egui::Rangef::new(LIST_FLOOR, widest))
            .default_size(list_width)
            .show(ui, |ui| list(ui, view, articles, &key, matching, [false, true]))
            .inner;
        egui::CentralPanel::default().show(ui, |ui| {
            pane(ui, view, articles, &key);
        });
    }
    opened.or(clicked)
}

/// One row of the list — the whole band is the door.
///
/// Measured before this: a row answered only on its title line, and the `10:53 · SOURCE`
/// line and the summary under it were dead. A blind sweep of the real window (156 clicks on
/// a grid) found Articles the worst page at 2.6 %; the row band answered 0–28 px and was
/// dead 32–80 px. A row is one target or it is a trap: the reader aims at the headline, at
/// the line under it, or at the teaser, and all three mean "read this".
///
/// The band is the row's text *plus* the air under it *plus* the hairline that closes it —
/// from the list's left edge to its right edge, never the title's own width. The interact
/// is registered **after** the text so it wins the click over the labels it covers, and its
/// fill is reserved **before** the text so the words are drawn on top of it.
fn row(ui: &mut egui::Ui, article: &Article, selected: bool) -> egui::Response {
    let id = ui.make_persistent_id(("article-row", article.id));
    let painter = ui.painter().clone();
    // No room for the band's fill yet: its height depends on how the text wraps, and a fill
    // painted after the words would cover them. `Shape::Noop` books the place in the paint
    // order and `set` fills it once the band is known.
    let fill_slot = painter.add(egui::Shape::Noop);
    let top = ui.cursor().top();

    let text = ui
        .scope(|ui| {
            // The strip the selection bar lives in, reserved on **every** row: the text must not
            // jump sideways the moment a row is selected.
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal_top(|ui| {
                ui.add_space(SELECTION_MARK);
                ui.vertical(|ui| {
                    ui.label(theme::body(&article.title));
                    ui.label(theme::dim(&format!(
                        "{} · {}",
                        when(&article.published),
                        if article.source.is_empty() { "unknown source" } else { &article.source }
                    )));
                    if !article.summary.is_empty() {
                        ui.label(theme::dim(&clip(&article.summary, 240)));
                    }
                });
            });
        })
        .response
        .rect;

    // The air under the text and the hairline are *inside* the band: a reader aiming at the
    // line between two rows is aiming at one of them, and the boundary belongs to the row
    // above it (the line is drawn in the middle of the air, so it is never on the seam where
    // two bands meet and `Rect::contains` cannot tell them apart).
    let band = egui::Rect::from_x_y_ranges(
        ui.max_rect().x_range(),
        top..=(text.bottom() + theme::space::SM),
    );
    let response = ui.interact(band, id, egui::Sense::click());

    // The selection is drawn as it was — `visuals.selection.bg_fill`, which the theme sets to
    // `SELECT`, at the one corner radius — only now it is the row's fill instead of the
    // title's. Hover and press use egui's own widget fills for a raised surface, so a row
    // that is about to be clicked says so.
    let fill = if selected {
        ui.visuals().selection.bg_fill
    } else if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active.weak_bg_fill
    } else if response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        painter.set(fill_slot, egui::Shape::rect_filled(band, theme::RADIUS, fill));
    }
    if selected {
        // The mark: the band's own edge, its whole height, in ink. A fill alone is 1.44:1 on
        // this page, so the selected row is carried by a shape as well as by ink.
        painter.rect_filled(
            egui::Rect::from_min_size(
                band.left_top(),
                egui::Vec2::new(SELECTION_MARK, band.height()),
            ),
            egui::CornerRadius::ZERO,
            theme::INK,
        );
    }
    painter.hline(
        band.x_range(),
        band.bottom() - theme::space::SM / 2.0,
        theme::soft_line(),
    );
    if response.has_focus() {
        // The ring is the row's, not the title's: what the reader needs to see is which row
        // Enter would open.
        painter.rect_stroke(
            band,
            theme::RADIUS,
            theme::focus_stroke(),
            egui::StrokeKind::Inside,
        );
    }
    // The band is taller than the text it holds, so the layout cursor is moved past all of
    // it by hand; the next row starts where this band ends.
    ui.advance_cursor_after_rect(band);
    response
}

/// The list, in whichever column the page gave it.
///
/// Answers the story a press on a row **opened on this frame**, and `None` on a frame where
/// the reader opened nothing. Not `view.selected`: the selection outlives the click, and a
/// page that reported it as an opening would report one every frame — the shell answers an
/// opening with [`reveal`], and a reveal every frame is a list that snaps back to the open
/// story under the reader's wheel (measured: the offset the wheel put at 300 px was pulled
/// back to the open row on the next frame, on every frame).
///
/// `fill` is the scroll area's `auto_shrink`: a `Panel` is only as wide (or tall) as the
/// content it is handed — the width requested with `default_size` is a *default*, and a
/// second frame shrinks the panel to the widest row it painted (measured: the 45 % share
/// collapsed to the 320 px floor before this line, and every row's highlight was as wide
/// as its title). Taking the space is also what makes a row a row: a title shorter than
/// the column still has the whole column as its click target.
fn list(
    ui: &mut egui::Ui,
    view: &mut ArticlesView,
    articles: &[Article],
    key: &str,
    matching: usize,
    fill: [bool; 2],
) -> Option<i64> {
    egui::ScrollArea::vertical()
        .auto_shrink(fill)
        .show(ui, |ui| {
            // The keyboard moved the ring last frame and the row it landed on may be below
            // the fold; that row asks the scroll area to bring it back on the frame that
            // draws it. The request lives in egui's memory for one frame, so it survives the
            // gap between the key press and the row it moved to.
            let scroll_to = ui.ctx().memory(|memory| {
                memory.data.get_temp::<i64>(scroll_request_id())
            });
            let mut focused: Option<i64> = None;
            let mut clicked: Option<i64> = None;
            for article in articles.iter().filter(|story| a_hit(story, key)) {
                let selected = view.selected == Some(article.id);
                let response = row(ui, article, selected);
                if response.clicked() {
                    view.selected = Some(article.id);
                    // The one story this frame opened. A click is the reader asking to read
                    // it, so the page answers it; the selection it leaves behind is not an
                    // answer, because the shell turns an answer into a reveal.
                    clicked = Some(article.id);
                    // Focus follows the click, so the ring shows on the row the reader just
                    // took and the arrow keys have somewhere to start. A click by keyboard
                    // arrives here too: egui turns Enter or Space on a focused clickable
                    // widget into a click (`FAKE_PRIMARY_CLICKED`, `context.rs`), so Enter is
                    // not a second code path that could disagree with the mouse.
                    response.request_focus();
                }
                if response.has_focus() {
                    focused = Some(article.id);
                }
                if scroll_to == Some(article.id) {
                    // Into view, not into the middle: `None` scrolls the least that shows the
                    // whole row and does nothing when it already is. Centring moved the list
                    // under a reader who had the row in front of them (measured: a press on
                    // the first, fully visible row yanked it from y = 267 to y = 567).
                    ui.scroll_to_rect(response.rect, None);
                }
            }
            if scroll_to.is_some() {
                ui.ctx()
                    .memory_mut(|memory| memory.data.remove::<i64>(scroll_request_id()));
            }
            if matching == 0 {
                ui.add_space(20.0);
                ui.label(theme::dim(
                    "Nothing matches. Clear the search, widen the window, or collect a story.",
                ));
            }
            // The selection follows the ring, and the ring is egui's: ↑ and ↓ are focus
            // navigation, so they move it, and a click focuses the row it hit. One rule
            // instead of two — an earlier version also moved the selection by hand and
            // requested focus, and the two disagreed by a row (measured: ↓ from the first
            // story left the ring on the third and Enter opened it).
            if let Some(row) = focused {
                view.selected = Some(row);
                // A row the arrows reached may be off the fold; a row the mouse reached is
                // already where the reader is looking, and centring it would move the list
                // under the pointer.
                let moved_by_key = ui.input(|input| {
                    input.key_pressed(egui::Key::ArrowDown)
                        || input.key_pressed(egui::Key::ArrowUp)
                });
                if moved_by_key {
                    let scroll_id = scroll_request_id();
                    ui.ctx()
                        .memory_mut(|memory| memory.data.insert_temp(scroll_id, row));
                }
            }
            // Esc is the way out of the open story without leaving the page — the reading
            // pane closes and the list stays where it is. Read here, outside any focus gate,
            // because egui **surrenders the focused widget on the Escape key in `begin_pass`**
            // (`egui-0.37.0/src/memory/mod.rs:597`): a handler behind `has_focus()` can never
            // see the key at all. It comes after the ring-to-selection rule above so that the
            // clear is the frame's last word; the ring went with the focus egui already
            // dropped, and Tab is the way back into the list when nothing is focused
            // (`memory/mod.rs:677`: the first widget that wants the focus takes it).
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) && view.selected.is_some() {
                view.selected = None;
            }
            clicked
        })
        .inner
}

/// The id the one-frame scroll request is stored under.
///
/// A constant, not `ui.make_persistent_id(…)`: the request is written by the shell (a story
/// opened from the Graph, a dot on the map) and read here inside the list panel's own `Ui`,
/// whose id chain is a different one. One slot is enough — there is one list on screen.
fn scroll_request_id() -> egui::Id {
    egui::Id::unique("articles-scroll-to")
}

/// The id the briefing card's measured height is kept under.
///
/// A slot in the context rather than a field on [`ArticlesView`]: the height is a fact about the
/// page the card was drawn on, not about what the reader chose, and a view that carried it would
/// have to be reset by every caller that builds one.
fn card_cost_id() -> egui::Id {
    egui::Id::unique("articles-briefing-cost")
}

/// Ask the list to bring one story into view on the frame it is drawn.
///
/// Called by the shell when a click opens a story from somewhere else: the row may be four
/// hundred stories down, and a page that opens at the top of a list whose open story is
/// below the fold looks exactly like a page that ignored the click.
pub fn reveal(ctx: &egui::Context, id: i64) {
    ctx.memory_mut(|memory| memory.data.insert_temp(scroll_request_id(), id));
}

/// The reading pane: the open story, or one sentence saying why there is none.
fn pane(ui: &mut egui::Ui, view: &ArticlesView, articles: &[Article], key: &str) {
    // A keystroke no longer throws the open story away. `response.changed()` used to
    // set `view.selected = None`, so narrowing a search to the story the reader was
    // reading emptied the pane — a filter that answers "nothing" about the one story
    // on screen. A story still listed keeps its pane; a story the search hides gets
    // the sentence below.
    let Some(selected) = view.selected else {
        ui.add_space(20.0);
        ui.label(theme::dim("Choose a story on the left to read it here."));
        return;
    };
    let Some(article) = articles
        .iter()
        .filter(|story| a_hit(story, key))
        .find(|article| article.id == selected)
    else {
        // Two different facts, two different sentences: the search hid the open
        // story (clear it and the story is back), or the window no longer holds it
        // at all. Saying the first one when the second is true would send the
        // reader to clear a search that has nothing to do with it.
        ui.add_space(20.0);
        if articles.iter().any(|article| article.id == selected) {
            ui.label(theme::dim(
                "The open story is not in this search. Clear the search to read it.",
            ));
        } else {
            ui.label(theme::dim("That story is no longer in the window."));
        }
        return;
    };
    ui.label(theme::page_title(&article.title));
    ui.add_space(2.0);
    ui.label(theme::dim(&format!(
        "{} · {} · {}",
        if article.source.is_empty() { "unknown source" } else { &article.source },
        when(&article.published),
        if article.country.is_empty() { "no place" } else { &article.country }
    )));
    ui.add_space(6.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        let body = if article.content.is_empty() { &article.summary } else { &article.content };
        if body.is_empty() {
            ui.label(theme::dim("No text was stored for this story."));
        } else {
            if !article.full_text {
                ui.label(theme::dim("Teaser only — the full text was not read."));
                ui.add_space(6.0);
            }
            ui.label(theme::body(body));
        }
        ui.add_space(10.0);
        if !article.url.is_empty() {
            // The address is archive text. It is gated (`crate::link`), and the window offers
            // two things with it: read the page **here**, in the window's own engine, or copy
            // the address. Never "hand it to the operating system": the window links no URL
            // opener, so a stored `file:`/custom scheme can never be launched by a click. The
            // host is shown, because a label that hides its destination is a label that lies.
            match crate::link::openable(&article.url) {
                Some(url) => {
                    // Two doors, in the order a reader means them: read the page here, in the
                    // window's own engine, or take the address away. A press on the first one
                    // only *remembers* the address — the shell opens it on this frame
                    // (`Atlas::open_source`), because a view does not open pages.
                    //
                    // The first door is `views::reader::source_link`, the same widget the two
                    // row lists host: its text carries the host it opens, so a link that
                    // leaves this pane says where it goes, and there is one gate in front of
                    // it (`crate::link::openable`) instead of one per page.
                    let _ = crate::views::reader::source_link(ui, url);
                    if ui.button(crate::link::label(url)).clicked() {
                        ui.ctx().copy_text(url.to_string());
                    }
                    ui.label(theme::dim(
                        "The page itself opens in this window.",
                    ));
                }
                None => {
                    ui.label(theme::dim(&format!(
                        "Source address (not a web address, not offered): {}",
                        clip(&article.url, 120)
                    )));
                }
            }
        }
    });
}


/// The reading pane's address area is the first of the Reader's three source links.
///
/// It is a *door*, not a text: pressing it fills the page-to-shell slot
/// (`views::reader::remember_source`) with the row's own address and nothing else happens —
/// the pane does not change, no page opens here, and the address is never handed to the
/// operating system. The copy button beside it is a different door: it asks for the
/// clipboard and leaves the slot alone, so a reader who only wants the address does not
/// also get a page.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::reader::harness;
    // The copy the pane asks for, under a name that does not spell `Command::`: the spec that
    // reads this crate as text cannot tell egui's output command from a process command, and
    // that rule ("an alias is the same call") is worth more than this test's import.
    use egui::OutputCommand as Output;

    /// One story with an address, as the archive hands it over.
    fn article() -> Article {
        Article {
            id: 3,
            title: "Ferry traffic resumes on the northern route".to_string(),
            source: "Harbour Post".to_string(),
            published: "2024-05-01T12:54:00Z".to_string(),
            url: "https://harbour.example/news/ferry".to_string(),
            summary: "The northern route reopened on Wednesday.".to_string(),
            full_text: true,
            ..Article::default()
        }
    }

    /// The pane, drawn on one frame: `show` is the page's public entry point, so the test
    /// walks the same path the window does (list, split, pane).
    fn pane_frame(ctx: &egui::Context, view: &mut ArticlesView, articles: &[Article]) -> harness::Painted {
        let (_, painted) = harness::drive(ctx, Vec::new(), |ui| {
            let _ = show(ui, view, articles, 72);
        });
        painted
    }

    #[test]
    fn the_address_area_offers_the_page_and_the_copy_as_two_doors() {
        let ctx = harness::fresh();
        let story = article();
        let mut view = ArticlesView { needle: String::new(), selected: Some(story.id) };
        // Two frames: the pane lives in a scroll area, which measures its content on the
        // frame it draws it.
        let _ = pane_frame(&ctx, &mut view, std::slice::from_ref(&story));
        let painted = pane_frame(&ctx, &mut view, std::slice::from_ref(&story));
        assert!(
            painted.has("Read the source"),
            "the address area does not offer the page:\n{}",
            painted.text
        );
        assert!(
            painted.has("harbour.example"),
            "the link does not say where it goes:\n{}",
            painted.text
        );

        let at = painted.at("Read the source");
        let (_answers, after) = harness::press(&ctx, at, |ui| {
            let _ = show(ui, &mut view, std::slice::from_ref(&story), 72);
        });
        assert_eq!(
            crate::views::reader::take_source(&ctx).as_deref(),
            Some("https://harbour.example/news/ferry"),
            "a press on the link did not hand the shell the row's address (after:\n{})",
            after.text
        );

        // The other door: the copy button asks for the clipboard and remembers nothing.
        let copy = after
            .rect_of("Copy harbour.example")
            .expect("the pane lost its copy button")
            .center();
        let (_answers, after_copy) = harness::press(&ctx, copy, |ui| {
            let _ = show(ui, &mut view, std::slice::from_ref(&story), 72);
        });
        assert_eq!(
            crate::views::reader::take_source(&ctx),
            None,
            "the copy button opened a page as well as copying it (after:\n{})",
            after_copy.text
        );
        assert!(
            after_copy
                .commands
                .iter()
                .any(|command| matches!(command, Output::CopyText(text) if text == "https://harbour.example/news/ferry")),
            "the copy button did not put the address on the clipboard"
        );
    }

    // ------------------------------------------------------------- the page, end to end

    /// Every filled rectangle one frame painted, with its colour.
    ///
    /// The shared harness collects the *strokes*, because the pages it was written for outline
    /// their state. A selection bar is a fill, so it needs its own reading.
    fn painted_fills(
        ctx: &egui::Context,
        view: &mut ArticlesView,
        articles: &[Article],
    ) -> (Vec<(egui::Rect, egui::Color32)>, harness::Painted) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 620.0),
            )),
            ..Default::default()
        };
        let mut boxes: Vec<(egui::Rect, egui::Color32)> = Vec::new();
        let mut painted = harness::Painted::default();
        let output = ctx.run_ui(input, |ui| {
            let _ = show(ui, view, articles, 72);
        });
        for clipped in &output.shapes {
            harvest(&clipped.shape, &mut boxes, &mut painted);
        }
        output.drop_without_applying_deltas();
        (boxes, painted)
    }

    /// The fills and the text of one shape tree, in one walk.
    fn harvest(
        shape: &egui::epaint::Shape,
        boxes: &mut Vec<(egui::Rect, egui::Color32)>,
        painted: &mut harness::Painted,
    ) {
        match shape {
            egui::epaint::Shape::Rect(rect) if rect.fill.a() > 0 && rect.rect.is_positive() => {
                boxes.push((rect.rect, rect.fill));
            }
            egui::epaint::Shape::Text(text) => {
                painted.text.push_str(text.galley.text());
                painted.text.push('\n');
                painted.labels.push((
                    text.galley.text().to_owned(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                ));
            }
            egui::epaint::Shape::Rect(rect) if rect.stroke.width > 0.0 => {
                painted.strokes.push(rect.rect);
            }
            egui::epaint::Shape::Vec(items) => {
                for item in items {
                    harvest(item, boxes, painted);
                }
            }
            _ => {}
        }
    }

    /// One frame of the page at a size the shared harness does not default to.
    ///
    /// The two-column split is a function of the width the page is *handed*, so a test that only
    /// ever draws at 900×620 cannot see the stacked layout at all.
    fn frame_at(
        ctx: &egui::Context,
        size: (f32, f32),
        view: &mut ArticlesView,
        articles: &[Article],
    ) -> harness::Painted {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size.0, size.1),
            )),
            ..Default::default()
        };
        let mut painted = harness::Painted::default();
        let output = ctx.run_ui(input, |ui| {
            let _ = show(ui, view, articles, 72);
        });
        for clipped in &output.shapes {
            harness::walk(&clipped.shape, &mut painted);
        }
        output.drop_without_applying_deltas();
        painted
    }

    /// The picture a reader is left looking at: two frames, because a scroll area measures its
    /// content on the frame after the one that handed it the content.
    fn settled(
        ctx: &egui::Context,
        view: &mut ArticlesView,
        articles: &[Article],
    ) -> harness::Painted {
        let _ = pane_frame(ctx, view, articles);
        pane_frame(ctx, view, articles)
    }

    /// `count` stories, each with its own title, source line, digest and body — so a painted
    /// line can be attributed to the row it came from, and the pane's body is never the row's
    /// summary (a test that finds "Digest 01." cannot tell the list from the pane otherwise).
    ///
    /// The last one is filed in Germany, which no other row names: that is how the pane's own
    /// subtitle is found without guessing at a column boundary.
    fn stories(count: i64) -> Vec<Article> {
        (1..=count)
            .map(|id| Article {
                id,
                title: format!("Story {id:02}"),
                source: format!("Wire {id:02}"),
                published: "2024-05-01T12:54:00Z".to_string(),
                summary: format!("Digest {id:02}."),
                content: format!("Body {id:02}."),
                country: if id == count { "Germany".to_string() } else { String::new() },
                full_text: true,
                ..Article::default()
            })
            .collect()
    }

    /// The rows a press wrote to a queue file, read back read-only — the file is one the test
    /// made, never the window's `data/requests.db`.
    fn queue_rows(file: &std::path::Path) -> Vec<(String, String)> {
        if !file.exists() {
            return Vec::new();
        }
        let connection =
            rusqlite::Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap_or_else(|problem| {
                    panic!("{} could not be opened: {problem}", file.display())
                });
        let mut statement = connection
            .prepare("SELECT kind, payload FROM requests ORDER BY id")
            .expect("the queue has no requests table");
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("the queue could not be read")
            .flatten()
            .collect()
    }

    /// A queue file of this test's own, in the temp directory.
    fn temp_queue(tag: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("atlas-articles-{}-{tag}.db", std::process::id()));
        // a previous failed run must not leave rows for this one to count
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path
    }

    /// (1) The search narrows the list in the frame the reader types, an empty answer says so in
    /// words, and Clear puts the list back.
    #[test]
    fn a_search_narrows_the_list_says_so_in_words_and_clear_restores_it() {
        let ctx = harness::fresh();
        let articles = stories(3);
        let mut view = ArticlesView::default();

        let all = settled(&ctx, &mut view, &articles);
        for row in ["Story 01", "Story 02", "Story 03"] {
            assert!(all.has(row), "the unfiltered list is missing {row}:\n{}", all.text);
        }
        assert!(
            all.has("3 stories in the last 72 h, newest first"),
            "the heading does not count the list:\n{}",
            all.text
        );

        view.needle = "DIGEST 02".to_string();
        let narrowed = settled(&ctx, &mut view, &articles);
        assert!(
            narrowed.has("Story 02"),
            "a search for the second story's digest hid it:\n{}",
            narrowed.text
        );
        assert!(
            !narrowed.has("Story 01") && !narrowed.has("Story 03"),
            "a search for one story painted the others:\n{}",
            narrowed.text
        );
        assert!(
            narrowed.has("1 of 3 stories match"),
            "the heading does not say how many matched:\n{}",
            narrowed.text
        );

        view.needle = "no such words anywhere".to_string();
        let empty = settled(&ctx, &mut view, &articles);
        assert!(
            empty.has("Nothing matches. Clear the search, widen the window, or collect a story."),
            "an empty search painted no sentence:\n{}",
            empty.text
        );
        assert!(
            empty.has("0 of 3 stories match"),
            "an empty search did not say the count was none:\n{}",
            empty.text
        );

        let at = empty.at("Clear");
        let (_answers, cleared) = harness::press(&ctx, at, |ui| {
            let _ = show(ui, &mut view, &articles, 72);
        });
        assert!(view.needle.is_empty(), "the Clear button left {:?} in the box", view.needle);
        for row in ["Story 01", "Story 02", "Story 03"] {
            assert!(cleared.has(row), "after Clear the list is missing {row}:\n{}", cleared.text);
        }
        assert!(
            cleared.has("3 stories in the last 72 h, newest first"),
            "after Clear the heading still counts a search:\n{}",
            cleared.text
        );
    }

    /// (2) One link, one copy button — and an address the window will not open is a sentence with
    /// no link and no copy on it.
    #[test]
    fn the_pane_offers_one_link_and_one_copy_and_refuses_every_other_address() {
        let ctx = harness::fresh();
        let story = article();
        let mut view = ArticlesView { needle: String::new(), selected: Some(story.id) };
        let painted = settled(&ctx, &mut view, std::slice::from_ref(&story));
        let links = painted
            .labels
            .iter()
            .filter(|(text, _)| text.starts_with("Read the source"))
            .count();
        let copies = painted
            .labels
            .iter()
            .filter(|(text, _)| text.starts_with("Copy "))
            .count();
        assert_eq!(links, 1, "the pane paints {links} source links:\n{}", painted.text);
        assert_eq!(copies, 1, "the pane paints {copies} copy buttons:\n{}", painted.text);
        assert!(
            painted.has("Read the source · harbour.example"),
            "the link does not carry the host it opens:\n{}",
            painted.text
        );

        // Everything the window refuses: other schemes, a credential form, a case-folded scheme,
        // a space, a control character, and a file path dressed as an address.
        for hostile in [
            "javascript:alert(1)",
            "file:///Users/someone/.ssh/id_ed25519",
            "HTTP://example.com/x",
            "https://user:pass@example.com/x",
            "https://example.com/a b",
            "https://example.com/\\u{202e}gnp.exe",
        ] {
            let mut bad = story.clone();
            bad.url = hostile.to_string();
            let mut view = ArticlesView { needle: String::new(), selected: Some(bad.id) };
            let painted = settled(&ctx, &mut view, std::slice::from_ref(&bad));
            assert!(
                !painted.has("Read the source"),
                "{hostile:?} was offered as a link:\n{}",
                painted.text
            );
            assert!(
                !painted.has("Copy "),
                "{hostile:?} was offered as something to copy:\n{}",
                painted.text
            );
            assert!(
                painted.has("Source address (not a web address, not offered)"),
                "{hostile:?} was refused without a sentence:\n{}",
                painted.text
            );
            assert!(
                painted.has(&clip(hostile, 120)),
                "the refused address is not shown, so the reader cannot even copy it by hand:\n{}",
                painted.text
            );
            assert_eq!(
                crate::views::reader::take_source(&ctx),
                None,
                "{hostile:?} reached the slot the shell opens"
            );
        }
    }

    /// (3) A story opened from another page is scrolled into view: the shell sets `selected` and
    /// calls `reveal`, and the row is on screen in the frames that follow.
    #[test]
    fn a_story_opened_from_another_page_is_scrolled_into_view() {
        let ctx = harness::fresh();
        let articles = stories(80);
        let mut view = ArticlesView::default();
        let start = settled(&ctx, &mut view, &articles);
        assert!(
            start.has("Digest 01"),
            "the first row of the list is not painted:\n{}",
            start.text
        );
        assert!(
            !start.has("Digest 80"),
            "the last row is already painted before anything asked for it, so the test proves \
             nothing:\n{}",
            start.text
        );

        // the shell's two lines, in the shell's order
        view.selected = Some(80);
        reveal(&ctx, 80);
        let mut painted = start;
        for _ in 0..10 {
            painted = settled(&ctx, &mut view, &articles);
        }

        assert!(
            painted.has("Digest 80"),
            "the revealed row is still below the fold:\n{}",
            painted.text
        );
        let shown = painted.rect_of("Digest 80").expect("the revealed row is not painted");
        assert!(
            shown.top() >= 0.0 && shown.bottom() <= 620.0,
            "the revealed row is at y {:.1}..{:.1}, outside the page",
            shown.top(),
            shown.bottom()
        );
        assert!(
            !painted.has("Digest 01"),
            "the list did not scroll: the first row is still painted:\n{}",
            painted.text
        );
        assert!(
            painted.has("Body 80."),
            "the pane does not show the story that was opened:\n{}",
            painted.text
        );
    }

    /// (4) The whole band is the door: the source line and the teaser open the story exactly as
    /// the headline does.
    #[test]
    fn a_press_anywhere_on_a_row_opens_that_row() {
        let ctx = harness::fresh();
        let articles = stories(3);
        let mut view = ArticlesView::default();
        let painted = settled(&ctx, &mut view, &articles);
        assert_eq!(view.selected, None, "a fresh page has a story open");

        for (where_, want) in [("Wire 02", 2_i64), ("Digest 03.", 3), ("Story 01", 1)] {
            let at = painted.at(where_);
            // The closure returns the page's own answer (`Option<i64>`), which is the
            // value `harness::press` collects one of per frame. The *press* frame is the one
            // that answers: a quiet frame reports nothing, because a standing selection is
            // not an opening (answering it every frame is the reveal-every-frame bug).
            let (answers, _after) = harness::press(&ctx, at, |ui| {
                show(ui, &mut view, &articles, 72)
            });
            assert_eq!(
                answers.iter().copied().flatten().next(),
                Some(want),
                "a press on {where_:?} answered {:?}, and that line is row {want}'s",
                answers
            );
            assert_eq!(
                view.selected,
                Some(want),
                "a press on {where_:?} did not select row {want}"
            );
        }
    }

    /// (3, the harder half) The reveal works on the frame the page is *first* drawn, which is
    /// what a switch from another page looks like: the list has no scroll state at all yet.
    #[test]
    fn a_reveal_on_the_first_frame_of_the_page_still_finds_the_row() {
        let ctx = harness::fresh();
        let articles = stories(80);
        let mut view = ArticlesView { needle: String::new(), selected: Some(80) };
        // the shell's order, before anything has drawn: select, then ask for the row
        reveal(&ctx, 80);
        let mut painted = harness::drive(&ctx, Vec::new(), |ui| {
            let _ = show(ui, &mut view, &articles, 72);
        })
        .1;
        for _ in 0..10 {
            painted = settled(&ctx, &mut view, &articles);
        }
        assert!(
            painted.has("Digest 80"),
            "the row a page opened on was never scrolled into view:\n{}",
            painted.text
        );
        let shown = painted.rect_of("Digest 80").expect("the revealed row is not painted");
        assert!(
            shown.top() >= 0.0 && shown.bottom() <= 620.0,
            "the revealed row is at y {:.1}..{:.1}, outside the page",
            shown.top(),
            shown.bottom()
        );
    }

    /// Every stroked rectangle one frame painted, with the width of its outline.
    ///
    /// The shared harness keeps the rects and drops the widths, which is enough for the pages that
    /// outline their state once. This page strokes a hairline under every row, so a ring can only
    /// be told from the furniture by the width it was drawn with.
    fn rings(
        ctx: &egui::Context,
        view: &mut ArticlesView,
        articles: &[Article],
        events: Vec<egui::Event>,
    ) -> Vec<(egui::Rect, f32)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 620.0),
            )),
            events,
            ..Default::default()
        };
        let mut out: Vec<(egui::Rect, f32)> = Vec::new();
        let output = ctx.run_ui(input, |ui| {
            let _ = show(ui, view, articles, 72);
        });
        fn collect(shape: &egui::epaint::Shape, out: &mut Vec<(egui::Rect, f32)>) {
            match shape {
                egui::epaint::Shape::Rect(rect) if rect.stroke.width > 0.0 => {
                    out.push((rect.rect, rect.stroke.width));
                }
                egui::epaint::Shape::Vec(items) => {
                    for item in items {
                        collect(item, out);
                    }
                }
                _ => {}
            }
        }
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        output.drop_without_applying_deltas();
        out
    }

    /// The focus ring is the clicked row's own band, read on the frame the ring is painted.
    ///
    /// The page-wide test (`tests/search_filter.rs`) compares the ring with a row measured on an
    /// earlier frame, and the shell under it grew a way-back line on the click frame, so it saw a
    /// ring one row below its title. This is the same claim inside one frame, where no shell is
    /// drawing: the ring covers the pressed row's title, source line and digest and stops at the
    /// rows above and below it.
    #[test]
    fn the_focus_ring_sits_on_the_row_it_belongs_to() {
        let ctx = harness::fresh();
        let articles = stories(3);
        let mut view = ArticlesView { needle: String::new(), selected: None };
        let page = settled(&ctx, &mut view, &articles);
        let at = page
            .rect_of("Story 02")
            .expect("the second row is not painted")
            .center();

        let ringed = |strokes: &[(egui::Rect, f32)]| -> Vec<egui::Rect> {
            strokes
                .iter()
                .filter(|(_, width)| (*width - theme::FOCUS_WIDTH).abs() < 0.01)
                .map(|(rect, _)| *rect)
                .collect()
        };
        let before = ringed(&rings(&ctx, &mut view, &articles, Vec::new()));
        assert!(
            before.is_empty(),
            "a ring was painted before anything was pressed: {before:?}"
        );

        let _ = rings(&ctx, &mut view, &articles, harness::pointer(at, true));
        let _ = rings(&ctx, &mut view, &articles, harness::pointer(at, false));
        // The ring is drawn from the focus the press asked for, and a press is reported at the
        // end of the pass that saw it: the frame after the release is the one a reader sees
        // with the ring on it (measured — press and release both paint no ring at all).
        let after = ringed(&rings(&ctx, &mut view, &articles, Vec::new()));
        assert_eq!(
            after.len(),
            1,
            "the press on the second row painted {} rings: {after:?}",
            after.len()
        );
        let ring = after[0];
        for needle in ["Story 02", "Wire 02", "Digest 02"] {
            let text = page.rect_of(needle).expect("the pressed row is not painted");
            assert!(
                ring.top() <= text.top() && ring.bottom() >= text.bottom(),
                "the ring at y {:.1}..{:.1} does not cover {needle:?} at y {:.1}..{:.1}",
                ring.top(),
                ring.bottom(),
                text.top(),
                text.bottom()
            );
        }
        let next = page.rect_of("Story 03").expect("the third row is not painted");
        let previous = page.rect_of("Story 01").expect("the first row is not painted");
        assert!(
            ring.bottom() <= next.top() + 0.5,
            "the ring reaches y {:.1}, into the row below it (y {:.1})",
            ring.bottom(),
            next.top()
        );
        assert!(
            ring.top() >= previous.bottom() - 0.5,
            "the ring starts at y {:.1}, inside the row above it (y {:.1})",
            ring.top(),
            previous.bottom()
        );
    }

    /// The selected row is carried by a shape and not only by ink: a 2 px bar in ink at the
    /// band's own left edge, on the selected row and on no other.
    ///
    /// The row is also filled (`theme::SELECT` under the page colour, measured 1.44:1), which is
    /// a selection a reader with low contrast cannot see; the bar is the graph's own mark, so the
    /// two lists say "this one" the same way.
    #[test]
    fn the_selected_row_carries_the_bar_and_the_others_do_not() {
        let ctx = harness::fresh();
        let articles = stories(3);
        let mut view = ArticlesView { needle: String::new(), selected: Some(2) };
        let _ = painted_fills(&ctx, &mut view, &articles);
        let (boxes, painted) = painted_fills(&ctx, &mut view, &articles);
        let bars: Vec<egui::Rect> = boxes
            .iter()
            .filter(|(rect, colour)| {
                *colour == theme::INK
                    && (rect.width() - SELECTION_MARK).abs() < 0.01
                    && rect.height() > 20.0
            })
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(
            bars.len(),
            1,
            "one selected row paints {} selection bars: {bars:?}",
            bars.len()
        );

        let bar = bars[0];
        let selected_digest =
            painted.rect_of("Digest 02").expect("the selected row is not painted");
        let next_digest = painted.rect_of("Digest 03").expect("the third row is not painted");
        assert!(
            bar.left() < selected_digest.left() && bar.left() > selected_digest.left() - 4.0,
            "the bar is at x = {:.1} and the row's text starts at {:.1}: it is not in the strip \
             reserved for it",
            bar.left(),
            selected_digest.left()
        );
        assert!(
            bar.top() < selected_digest.top() && bar.bottom() > selected_digest.bottom(),
            "the bar at y {:.1}..{:.1} does not cover the selected row's text \
             ({:.1}..{:.1})",
            bar.top(),
            bar.bottom(),
            selected_digest.top(),
            selected_digest.bottom()
        );
        assert!(
            bar.bottom() < next_digest.top(),
            "the bar at y {:.1}..{:.1} runs into the third row, whose text ends at {:.1}",
            bar.top(),
            bar.bottom(),
            next_digest.top()
        );

        // nothing selected: no bar anywhere
        view.selected = None;
        let _ = painted_fills(&ctx, &mut view, &articles);
        let (boxes, _) = painted_fills(&ctx, &mut view, &articles);
        let bars = boxes
            .iter()
            .filter(|(rect, colour)| {
                *colour == theme::INK
                    && (rect.width() - SELECTION_MARK).abs() < 0.01
                    && rect.height() > 20.0
            })
            .count();
        assert_eq!(bars, 0, "with nothing selected the page still paints {bars} bars");

        // another row: the bar goes with the selection
        view.selected = Some(3);
        let _ = painted_fills(&ctx, &mut view, &articles);
        let (boxes, _) = painted_fills(&ctx, &mut view, &articles);
        let moved = boxes
            .iter()
            .find(|(rect, colour)| {
                *colour == theme::INK
                    && (rect.width() - SELECTION_MARK).abs() < 0.01
                    && rect.height() > 20.0
            })
            .map(|(rect, _)| *rect)
            .unwrap_or_else(|| panic!("the third row was selected and no bar was painted"));
        assert!(
            moved.top() > bar.top(),
            "the bar did not move with the selection: it is still at y = {:.1}",
            moved.top()
        );
    }

    /// (4, the seam) A row's band ends where the next row's begins: the air under the text, the
    /// hairline in it and the strip the layout would leave beneath it all belong to a row.
    ///
    /// Measured before this: `advance_cursor_after_rect` leaves `item_spacing` (8 px) under every
    /// band, and the next row started below that — so 8 px of every row were a press that opened
    /// nothing, which is the same defect as the dead half of a row, one layer down.
    #[test]
    fn there_is_no_dead_strip_between_two_rows() {
        // Each press starts on a virgin page: the ring rule ("the selection follows the focus")
        // answers with the row the *previous* press focused, so a measurement that reused a page
        // would report a row that no press had touched.
        let measure = |ctx: &egui::Context| -> (f32, f32, f32) {
            let mut view = ArticlesView::default();
            let painted = settled(ctx, &mut view, &stories(3));
            let digest = painted.rect_of("Digest 01").expect("the first row is not painted");
            let next = painted.rect_of("Story 02").expect("the second row is not painted");
            // The first band's own end, as `row` builds it: the text plus one `SM` of air.
            let seam = digest.bottom() + theme::space::SM;
            (digest.center().x, seam, next.top())
        };

        let ctx = harness::fresh();
        let (x, seam, next) = measure(&ctx);
        assert!(
            next > seam,
            "the second row starts at y = {next:.1} and the first ends at {seam:.1} — there is \
             no strip between them to test"
        );
        println!(
            "the first band ends at y = {seam:.1} and the second row starts at {next:.1}: \
             {} px between two rows",
            next - seam
        );

        // Every point from one row's text to the other's opens a row: the strip the layout
        // leaves between two bands is covered by egui's interaction radius, so a press there is
        // a press on a row and never on nothing.
        let mut dead: Vec<f32> = Vec::new();
        let mut answered: Vec<(f32, Option<i64>)> = Vec::new();
        for step in 0..=(theme::space::SM as i32 + 2) {
            let y = seam - 1.0 + step as f32;
            let ctx = harness::fresh();
            let mut view = ArticlesView::default();
            let articles = stories(3);
            let (answers, _after) = harness::press(&ctx, egui::pos2(x, y), |ui| {
                show(ui, &mut view, &articles, 72)
            });
            let got = answers.iter().copied().flatten().next();
            answered.push((y, got));
            if got != Some(1) && got != Some(2) {
                dead.push(y);
            }
        }
        assert!(
            dead.is_empty(),
            "the strip between two rows answered nothing at y = {dead:?}; the whole sweep was \
             {answered:?}"
        );
        // and the sweep does reach both rows, so it is not answering one row everywhere
        assert!(
            answered.first().map(|(_, row)| *row) == Some(Some(1)),
            "the last point of the first row answered {:?}",
            answered.first()
        );
        assert!(
            answered.last().map(|(_, row)| *row) == Some(Some(2)),
            "the first point of the second row answered {:?}",
            answered.last()
        );
        println!("the sweep across the seam answered {answered:?}");
    }

    /// A briefing of the whole window, with one bullet, a watermark behind the archive and a
    /// line a test can look for.
    fn window_briefing() -> crate::data::Briefing {
        crate::data::Briefing {
            scope: "window".to_string(),
            key: String::new(),
            hours: 72,
            watermark: 4,
            created: "2024-05-01T12:00:00Z".to_string(),
            line: "The window is about ports.".to_string(),
            bullets: vec!["Grain is moving [Article #2]".to_string()],
            articles: 3,
            model: "test".to_string(),
            ms: 10,
        }
    }

    /// The briefing card is drawn where the page can hold it whole, and not drawn at all where it
    /// cannot — a half-shown card hides its action and the sentence its action writes.
    ///
    /// The two sizes are the ends of the audit's range: 700×520 is the narrow window the user's
    /// "the graph is not even full screen" is about, and 900×620 is the smallest size at which
    /// this page has ever held a card. Measured before the rule: at 700×520 the card took 137 px,
    /// the columns were handed 200 px, the list took its floor and the story was left 48 px — no
    /// body at all. Now the card is skipped there and the story column gets its floor.
    #[test]
    fn the_briefing_card_is_drawn_where_it_fits_whole_and_skipped_where_it_cannot_be() {
        let articles = stories(3);
        let briefing = window_briefing();
        let ask = crate::views::brief::Ask::new("window", "", "the window", 72, Some(&briefing), 5);
        let at = |size: (f32, f32)| -> harness::Painted {
            let ctx = harness::fresh();
            let queue = temp_queue("card-fits");
            // One story open, so the pane has a body to paint: a page with nothing selected shows
            // the pane's invitation and the reading column is not exercised at all.
            let mut view = ArticlesView { needle: String::new(), selected: Some(2) };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(size.0, size.1),
                )),
                ..Default::default()
            };
            let mut painted = harness::Painted::default();
            // Two frames: the split is a panel, and a panel measures its content on the frame
            // after the one that hands it the content.
            for _ in 0..2 {
                let input = egui::RawInput { screen_rect: input.screen_rect, ..input.clone() };
                painted = harness::Painted::default();
                let output = ctx.run_ui(input, |ui| {
                    let _ = show_briefed(ui, &mut view, &articles, 72, Some(&ask), Some(&queue));
                });
                for clipped in &output.shapes {
                    harness::walk(&clipped.shape, &mut painted);
                }
                output.drop_without_applying_deltas();
            }
            painted
        };

        let narrow = at((700.0, 520.0));
        assert!(
            !narrow.has("The window is about ports."),
            "the card is drawn at 700×520, where it leaves no story column:\n{}",
            narrow.text
        );
        assert!(
            narrow.has("Body 02."),
            "the story the page was opened on is not painted at 700×520:\n{}",
            narrow.text
        );

        let roomy = at((900.0, 620.0));
        assert!(
            roomy.has("The window is about ports."),
            "the card is skipped at 900×620, where the page can hold it whole:\n{}",
            roomy.text
        );
        assert!(
            roomy.has("3 stories in 72 h"),
            "the card is drawn at 900×620 without its footer:\n{}",
            roomy.text
        );
        assert!(
            roomy.has("Body 02."),
            "the card took the story column at 900×620:\n{}",
            roomy.text
        );
    }

    /// (5) The briefing card draws the sentence, its footer and its staleness, and Refresh writes
    /// **one** row to the queue — the wiring, not the picture.
    #[test]
    fn the_briefing_card_draws_and_refresh_writes_exactly_one_request_row() {
        let ctx = harness::fresh();
        let queue = temp_queue("refresh");
        let articles = stories(3);
        let briefing = window_briefing();
        let ask = crate::views::brief::Ask::new("window", "", "the window", 72, Some(&briefing), 5);
        let mut view = ArticlesView::default();
        let draw = |ui: &mut egui::Ui, view: &mut ArticlesView| {
            let _ = show_briefed(ui, view, &articles, 72, Some(&ask), Some(&queue));
        };
        let (_, _) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        assert!(
            painted.has("The window is about ports."),
            "the briefing's sentence is not on the page:\n{}",
            painted.text
        );
        assert!(
            painted.has("3 stories in 72 h"),
            "the card's footer is missing:\n{}",
            painted.text
        );
        assert!(
            painted.has("the archive has newer stories — refresh"),
            "a stale briefing does not say so:\n{}",
            painted.text
        );
        assert!(painted.has("Refresh"), "the card offers no Refresh:\n{}", painted.text);
        assert_eq!(
            queue_rows(&queue).len(),
            0,
            "a card nobody pressed wrote a request"
        );

        let at = painted.at("Refresh");
        let (_answers, _after_press) = harness::press(&ctx, at, |ui| draw(ui, &mut view));
        // `harness::press` answers with the quiet frame after the release: the frame a reader
        // reads the card's answer on, and the one the sentence the press wrote is measured in.
        let (_, after) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        let rows = queue_rows(&queue);
        assert_eq!(rows.len(), 1, "one press on Refresh wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, crate::views::brief::KIND, "the wrong kind was queued");
        let payload: serde_json::Value =
            serde_json::from_str(&rows[0].1).expect("the payload is not JSON");
        assert_eq!(payload["scope"], "window");
        assert_eq!(payload["key"], "");
        assert_eq!(payload["hours"], 72);
        assert!(
            payload["why"].as_str().unwrap_or("").contains("the window"),
            "the request does not say what it is about: {}",
            rows[0].1
        );
        assert!(
            after.has("asked the agent to write a briefing about the window"),
            "the card did not say what the press did:\n{}",
            after.text
        );
        // one press, one row: the frame after the press writes nothing
        let (_, _) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        assert_eq!(queue_rows(&queue).len(), 1, "the request was written more than once");
        let _ = std::fs::remove_file(&queue);
    }

    /// (5, empty state) With no briefing the card still draws its scope and its one action, and
    /// that action writes **one** row; a page with no queue says so in words instead of failing
    /// silently.
    #[test]
    fn an_empty_card_still_asks_and_a_window_with_no_queue_says_nothing_was_asked() {
        let ctx = harness::fresh();
        let queue = temp_queue("write");
        let articles = stories(2);
        let ask = crate::views::brief::Ask::new("window", "", "the window", 72, None, 0);
        let mut view = ArticlesView::default();
        let draw = |ui: &mut egui::Ui, view: &mut ArticlesView| {
            let _ = show_briefed(ui, view, &articles, 72, Some(&ask), Some(&queue));
        };
        let (_, _) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        assert!(painted.has("Briefing"), "the empty card is not drawn:\n{}", painted.text);
        assert!(
            painted.has("the window"),
            "the card does not name the scope it is about:\n{}",
            painted.text
        );
        let actions = painted
            .labels
            .iter()
            .filter(|(text, _)| text.trim() == "Refresh")
            .count();
        assert_eq!(
            actions, 1,
            "the empty card offers {actions} actions, and it has exactly one: {}",
            painted.text
        );
        let at = painted.at("Refresh");
        let (_answers, _after) = harness::press(&ctx, at, |ui| draw(ui, &mut view));
        let rows = queue_rows(&queue);
        assert_eq!(rows.len(), 1, "the empty card wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, crate::views::brief::KIND);
        let _ = std::fs::remove_file(&queue);

        // a page with no queue: the action is drawn, the press is answered in words, and nothing
        // is written anywhere
        let ctx = harness::fresh();
        let ask = crate::views::brief::Ask::new("window", "", "the window", 72, None, 0);
        let mut view = ArticlesView::default();
        let draw = |ui: &mut egui::Ui, view: &mut ArticlesView| {
            let _ = show_briefed(ui, view, &articles, 72, Some(&ask), None);
        };
        let (_, _) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        let (_, painted) = harness::drive(&ctx, Vec::new(), |ui| draw(ui, &mut view));
        let at = painted.at("Refresh");
        let (_answers, after) = harness::press(&ctx, at, |ui| draw(ui, &mut view));
        assert!(
            after.has("this window has no request queue open, so nothing was asked"),
            "a queue-less press said nothing:\n{}",
            after.text
        );
    }

    /// (6) At the window the app opens (900×620) two columns hold — the list never falls under
    /// its floor, the pane never under its own — nothing is painted outside the page, and below
    /// split floor the page stacks instead of squeezing two columns into one.
    #[test]
    fn at_nine_hundred_by_six_twenty_the_columns_hold_and_nothing_overflows() {
        let ctx = harness::fresh();
        let articles = stories(8);
        let mut view = ArticlesView { needle: String::new(), selected: Some(8) };
        let painted = settled(&ctx, &mut view, &articles);
        for (text, rect) in &painted.labels {
            assert!(
                rect.min.x >= -1.0 && rect.max.x <= 901.0,
                "{text:?} is painted at x {:.1}..{:.1} in a 900 px page",
                rect.min.x,
                rect.max.x
            );
            assert!(
                rect.min.y >= -1.0 && rect.max.y <= 621.0,
                "{text:?} is painted at y {:.1}..{:.1} in a 620 px page",
                rect.min.y,
                rect.max.y
            );
        }
        let pane = painted.rect_of("Germany").expect("the pane's own line is not painted");
        assert!(
            pane.min.x >= LIST_FLOOR,
            "the reading pane starts at x = {:.1}, under the list's {LIST_FLOOR} px floor",
            pane.min.x
        );
        assert!(
            900.0 - pane.min.x >= PANE_FLOOR,
            "the reading pane is {:.1} px wide, under its {PANE_FLOOR} px floor",
            900.0 - pane.min.x
        );

        // A story whose title and teaser have no space to break at is the case a column has to
        // survive: the text wraps inside the column or it is cut there, but it may not spill.
        let mut stubborn = stories(2);
        stubborn[0].title = "Supercalifragilisticexpialidocious".repeat(12);
        stubborn[0].summary = "https://example.com/".to_string() + &"a".repeat(400);
        let ctx = harness::fresh();
        let mut view = ArticlesView { needle: String::new(), selected: Some(1) };
        let _ = pane_frame(&ctx, &mut view, &stubborn);
        let painted = pane_frame(&ctx, &mut view, &stubborn);
        for (text, rect) in &painted.labels {
            assert!(
                rect.min.x >= -1.0 && rect.max.x <= 901.0,
                "an unbreakable {:.20}… is painted at x {:.1}..{:.1} in a 900 px page",
                text,
                rect.min.x,
                rect.max.x
            );
        }

        // 700 px is below the split floor: the page stacks, and the pane takes the whole width
        let ctx = harness::fresh();
        let mut view = ArticlesView { needle: String::new(), selected: Some(8) };
        let _ = frame_at(&ctx, (700.0, 520.0), &mut view, &articles);
        let stacked = frame_at(&ctx, (700.0, 520.0), &mut view, &articles);
        for (text, rect) in &stacked.labels {
            assert!(
                rect.min.x >= -1.0 && rect.max.x <= 701.0,
                "{text:?} is painted at x {:.1}..{:.1} in a 700 px page",
                rect.min.x,
                rect.max.x
            );
            assert!(
                rect.min.y >= -1.0 && rect.max.y <= 521.0,
                "{text:?} is painted at y {:.1}..{:.1} in a 520 px page",
                rect.min.y,
                rect.max.y
            );
        }
        let pane = stacked.rect_of("Germany").expect("the stacked pane is not painted");
        assert!(
            pane.min.x < PANE_FLOOR,
            "at 700 px the pane still starts at x = {:.1}: the page kept two columns it can't \
             hold",
            pane.min.x
        );
    }


    // ------------------------------------------------------------------ the wheel
    //
    // The bug these pin: the page answered its **standing** selection as if a press had just
    // opened it, the shell turned every answer into `reveal`, and the list was dragged back to
    // the open story on every frame — so the wheel could not move it, and a press on the first
    // row threw the list to the top.

    /// How far a list at rest may still tick between two frames: a settling smooth scroll moves
    /// in whole pixels, and the last fraction of a pixel is not the bug. The bug moved the list
    /// by hundreds of pixels — back to the row the page thought it had just opened.
    const SETTLED_PX: f32 = 4.0;

    /// How far, over a dozen quiet frames, a list may drift from where the wheel left it. A list
    /// that is pulled back towards a standing selection moves slowly at first (egui's smooth
    /// scroll eases in), so three frames see nothing; twelve frames see the pull. No healthy list
    /// moves at all here, so the bound only has to stay under one pull step.
    const REVEAL_PULL_PX: f32 = 24.0;

    /// One turn of the wheel over `at`: the pointer goes there first, as a mouse arrives.
    fn wheel(at: egui::Pos2, dy: f32) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }

    /// One frame, with the shell's own rule applied: a story the page answered is the story
    /// the shell reveals (`app.rs`: `open_story(opened)` → `views::articles::reveal`).
    ///
    /// The rule is transcribed rather than invented. A view test that skipped it could not see
    /// the loop a reader sees, and the loop is where the bug lived.
    fn shell_frame(
        ctx: &egui::Context,
        view: &mut ArticlesView,
        articles: &[Article],
        events: Vec<egui::Event>,
    ) -> harness::Painted {
        let (answer, painted) = harness::drive(ctx, events, |ui| show(ui, view, articles, 72));
        if let Some(id) = answer {
            reveal(ctx, id);
        }
        painted
    }

    /// Drive frames until `row` stops moving, and answer its top.
    ///
    /// egui animates a wheel's offset over several frames, so the only honest place to read the
    /// list's position is where it has come to rest. `events` are handed to the first frame.
    ///
    /// "At rest" is three frames within a pixel of each other, not two identical ones: a galley
    /// is positioned on whole pixels, so a scroll that has effectively stopped can still tick
    /// between 163.0 and 162.0 while the last fraction settles.
    fn rest(
        ctx: &egui::Context,
        view: &mut ArticlesView,
        articles: &[Article],
        row: &str,
        events: Vec<egui::Event>,
    ) -> (harness::Painted, f32) {
        const ROUNDING: f32 = 1.5;
        let mut painted = shell_frame(ctx, view, articles, events);
        let mut seen: Vec<f32> = Vec::with_capacity(4);
        for _ in 0..80 {
            match painted.rect_of(row) {
                Some(rect) => {
                    seen.push(rect.top());
                    if seen.len() >= 3 {
                        let recent = &seen[seen.len() - 3..];
                        let high = recent.iter().copied().fold(f32::MIN, f32::max);
                        let low = recent.iter().copied().fold(f32::MAX, f32::min);
                        if high - low <= ROUNDING {
                            return (painted, rect.top());
                        }
                    }
                }
                None => seen.clear(),
            }
            painted = shell_frame(ctx, view, articles, Vec::new());
        }
        panic!(
            "{row:?} never came to rest; the last frame painted:\n{}",
            painted.text
        );
    }
    fn top_of(painted: &harness::Painted, row: &str, frame: &str) -> f32 {
        painted
            .rect_of(row)
            .unwrap_or_else(|| panic!("{frame}: {row:?} is not painted:\n{}", painted.text))
            .top()
    }

    #[test]
    fn the_offset_the_wheel_put_the_list_at_survives_a_repaint() {
        let ctx = harness::fresh();
        let articles = stories(20);
        let mut view = ArticlesView::default();
        let at = egui::pos2(200.0, 300.0);
        let (_, before) = rest(&ctx, &mut view, &articles, "Story 03", Vec::new());
        // the reader turns the wheel over the list
        let (_, after) = rest(&ctx, &mut view, &articles, "Story 03", wheel(at, -100.0));
        assert!(
            before - after > 50.0,
            "the wheel moved the row by {:.1} px, so there is no offset to keep",
            before - after
        );
        // three frames with nothing happening: the list stays where the wheel left it
        for frame in 1..=3 {
            let painted = shell_frame(&ctx, &mut view, &articles, Vec::new());
            let y = top_of(&painted, "Story 03", &format!("quiet frame {frame}"));
            assert!(
                (y - after).abs() <= SETTLED_PX,
                "quiet frame {frame}: the list moved from y = {after:.1} to y = {y:.1} with \
                 nothing touching it (more than a settling list ticks)"
            );
        }
    }

    #[test]
    fn opening_a_story_leaves_the_list_where_the_reader_scrolled_it() {
        let ctx = harness::fresh();
        let articles = stories(20);
        let mut view = ArticlesView::default();
        let (painted, scrolled) = rest(
            &ctx,
            &mut view,
            &articles,
            "Story 04",
            wheel(egui::pos2(200.0, 300.0), -100.0),
        );
        // the reader presses the row in front of them
        let target = painted.at("Wire 04");
        let _ = shell_frame(&ctx, &mut view, &articles, harness::pointer(target, true));
        let _ = shell_frame(&ctx, &mut view, &articles, harness::pointer(target, false));
        assert_eq!(view.selected, Some(4), "the press did not open story 4");
        let mut last = painted;
        for frame in 1..=3 {
            last = shell_frame(&ctx, &mut view, &articles, Vec::new());
            let y = top_of(&last, "Story 04", &format!("after the press, frame {frame}"));
            assert!(
                (y - scrolled).abs() <= SETTLED_PX,
                "frame {frame}: opening a story moved the list from y = {scrolled:.1} to \
                 y = {y:.1} (more than a settling list ticks)"
            );
        }
        assert!(
            last.has("Body 04."),
            "the pane did not open the story that was pressed:\n{}",
            last.text
        );
    }

    #[test]
    fn a_reveal_brings_its_story_into_view_once_and_never_again() {
        let ctx = harness::fresh();
        let articles = stories(80);
        let mut view = ArticlesView::default();
        let start = shell_frame(&ctx, &mut view, &articles, Vec::new());
        assert!(
            !start.has("Digest 80"),
            "the last row is already painted before anything asked for it, so the test proves \
             nothing:\n{}",
            start.text
        );

        // the shell's two lines, in the shell's order: select, then ask
        view.selected = Some(80);
        reveal(&ctx, 80);
        let (shown, y78) = rest(&ctx, &mut view, &articles, "Story 78", Vec::new());
        let revealed = shown
            .rect_of("Digest 80")
            .expect("the revealed row is not painted");
        assert!(
            revealed.top() >= 0.0 && revealed.bottom() <= 620.0,
            "the revealed row is at y {:.1}..{:.1}, outside the page",
            revealed.top(),
            revealed.bottom()
        );

        // the reader scrolls away from it, and the reveal does not come back for a second dose
        let (_, after) = rest(
            &ctx,
            &mut view,
            &articles,
            "Story 78",
            wheel(egui::pos2(200.0, 300.0), 150.0),
        );
        assert!(
            after - y78 > 100.0,
            "the wheel moved the list by {:.1} px after the reveal, not by the 150 it was given",
            after - y78
        );
        // twelve quiet frames: a reveal that has already done its work does not come back, while
        // one that runs every frame drags the list towards its story a little further each frame
        for frame in 1..=12 {
            let painted = shell_frame(&ctx, &mut view, &articles, Vec::new());
            let y = top_of(&painted, "Story 78", &format!("after the wheel, frame {frame}"));
            assert!(
                (y - after).abs() <= REVEAL_PULL_PX,
                "frame {frame}: the reveal pulled the list from y = {after:.1} to y = {y:.1}, \
                 although nothing asked for story 80 again"
            );
        }
    }
}
