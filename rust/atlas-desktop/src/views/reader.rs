//! The Reader page: one story's source, in the window rather than in a browser.
//!
//! The page has **two ways to show the same address**, and it never decides which one the
//! reader gets:
//!
//! * **With an engine** (`view: Some`), a system web view paints the page: WKWebView on macOS,
//!   WebView2 on Windows, both of which ship with the operating system. This module draws the
//!   toolbar, a one-pixel outline where the page will be, and the host — and hands the
//!   rectangle to the engine ([`crate::webview::Page::set_bounds`]). It draws **no scroll
//!   area** around that rectangle, on purpose: the engine scrolls its own content, and an
//!   egui scroll area on top of it would eat the wheel before the page ever saw it (the same
//!   defect the Graph page had). `tests/ui_layout.rs` counts scroll areas for that reason.
//! * **Without one** (`view: None` — a Linux build, or a machine where the engine could not be
//!   created), the stored text *is* the page: the title, `source · when` in dim, the
//!   paragraphs in a scroll area this page owns, and the address as a copy link. Two sentences
//!   keep that honest instead of pretending: [`ENGINE_SENTENCE`] says what this build cannot
//!   render, and [`TEASER_SENTENCE`] says when the archive never read the whole page.
//!
//! What the page decides and what it does not:
//!
//! * It **answers**; it does not act. A press on Reload, Ask about this, Tag it or the back
//!   arrow leaves as an [`Action`], and the shell (`app.rs`) decides what to do with it — the
//!   same division every view in this crate keeps.
//! * It never hands a URL to the operating system. The address it shows is gated by
//!   [`crate::link::openable`] and the action it offers is *copy*: the reader decides what to
//!   do with it. The engine receives only addresses the shell already gated
//!   ([`crate::webview::allowed`]).
//! * It never picks a colour, a size or a spacing value: everything comes from [`crate::theme`].
//!
//! # The three "Read the source" links
//!
//! Articles, Overview and Monitors each gain one line that asks for this page. Rather than
//! change three function signatures that `app.rs` calls every frame, the link leaves its
//! answer in the context's own store — the mechanism this crate already uses for "which row is
//! open" (`views::topics::open_topic`, `views::monitors::open_monitor`): [`remember_source`]
//! writes, and the shell calls [`take_source`] once per frame. A `&mut Option<String>` would
//! be the alternative; it touches five call sites to carry one string, and the slot is what the
//! two existing page-to-shell facts already use.

use egui::{CornerRadius, Rect, Sense, StrokeKind};

use crate::theme;
use crate::views::{clip, when};

/// What this page is, said once and plainly: the archive's words, not the page.
///
/// It is painted **only** when no engine view was handed in, so the sentence must be true both
/// when the platform has no system web view and when it has one that declined this address —
/// which is why it says what the reader is looking at instead of why. It used to say "this
/// build reads pages as text", a sentence that stopped being true the day the window grew its
/// own web-view page (`crate::webview`): with an engine, the page *is* the page, and this line
/// is what a reader gets when it is not.
pub const ENGINE_SENTENCE: &str = "This is the archive's copy of the page's words, not the page \
                                   itself; the page itself needs the system web view.";

/// The archive holds a teaser, not the article. Said in the page, not in a tooltip: a reader
/// who reaches the end of four paragraphs deserves to know that the page has more.
pub const TEASER_SENTENCE: &str = "Teaser only — the full text was not read.";

/// The archive's line above the text when a newer version of the page has arrived since this
/// one was read.
pub const STALE_LINE: &str = "the archive has a newer version of this page";

/// One story as the Reader shows it: the columns the page needs, already read.
///
/// Every field is borrowed from the archive row the shell read (`data::Article`); the page
/// does not open anything, so a fixture and a live row are the same value here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Story {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub published: String,
    pub url: String,
    pub text: String,
    pub full_text: bool,
}

impl Story {
    /// The archive row, in the shape the page draws. One place, so a caller cannot fill six
    /// fields in a different order than the page reads them.
    pub fn of(article: &crate::data::Article) -> Story {
        // The pane has always shown the body when there is one and the summary when there is
        // not; the Reader keeps that rule rather than inventing a second one.
        let text = if article.content.trim().is_empty() {
            article.summary.clone()
        } else {
            article.content.clone()
        };
        Story {
            id: article.id,
            title: article.title.clone(),
            source: article.source.clone(),
            published: article.published.clone(),
            url: article.url.clone(),
            text,
            full_text: article.full_text,
        }
    }
}

/// What a press on the page's own controls answers.
///
/// The page knows no `Atlas` and no engine: it reports the press, and the shell decides. That
/// is why the enum has no payload — the story is the shell's, not the page's, and a `Back` that
/// carried a URL would be the page inventing a history it does not keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// The engine's own history: back to the page before this one.
    Back,
    /// Leave the source page altogether — the page the reader came from, or the list.
    Close,
    Refresh,
    Ask,
    Tag,
}

/// The words on the three links, in one place because a test asserts them and a failure
/// message quotes them.
const RELOAD: &str = "Reload";
const ASK: &str = "Ask about this";
const TAG: &str = "Tag it";
/// The one glyph this page draws: a left arrow, and the only "icon" the toolbar has.
const ARROW: &str = "←";
/// The way out of the page, in words. A child web view is not a widget: it paints over the
/// window and it has no frame of its own, so the page has to offer the door out itself.
const CLOSE: &str = "Close";

/// Draw the Reader page inside the box it is given, and answer what was pressed.
///
/// `view` is the engine the shell opened for this address, if there is one: `Some` means the
/// engine paints the page area and the page draws only its outline, `None` means the stored
/// text is the page. `stale` is the shell's answer to "has the archive read a newer version of
/// this page since?" — the page only says so.
pub fn show(
    ui: &mut egui::Ui,
    story: &Story,
    view: Option<&mut crate::webview::Page>,
    stale: bool,
) -> Action {
    // The address the toolbar names is the one the **page is on**, not the story's: a page can
    // follow a link or a redirect anywhere inside `crate::webview::allowed`'s rule, and a
    // toolbar that kept printing the story's host would be naming a page the engine left.
    // Without an engine there is no page but the archive's copy, so the story's address is the
    // address. `Page::url` refreshes itself on the bounds call the frame makes anyway.
    let shown = match view.as_ref() {
        Some(page) => page.url(),
        None => &story.url,
    };
    let host = host(shown);
    let mut action = Action::None;
    // With an engine the reader is standing on a page that engine loaded, so there is
    // somewhere to go back to and the arrow appears. Without one this page keeps no history of
    // its own (the shell's back stack is the shell's), so an arrow here would be a control the
    // page cannot honour. **This is the only signal the contract gives the page for "there is
    // somewhere to go"** — see the module note.
    let can_back = view.is_some();
    action = toolbar(ui, &host, can_back).unwrap_or(action);
    ui.add_space(theme::space::SM);

    match view {
        Some(page) => {
            // The engine's rectangle: everything the toolbar left. No scroll area (the engine
            // scrolls), no fill (the engine paints), no text (it would be covered) — one
            // hairline so the reader sees where the page begins, and the bounds handed over so
            // the engine paints exactly there.
            let rect = ui.allocate_response(ui.available_size(), Sense::hover()).rect;
            ui.painter().rect_stroke(
                rect,
                CornerRadius::ZERO,
                theme::line(),
                StrokeKind::Inside,
            );
            page.set_bounds(rect);
        }
        None => {
            // The text page owns its scroll: it is prose of unknown length and it is the only
            // thing in the window that scrolls here.
            egui::ScrollArea::vertical()
                .id_salt("reader-text")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    text_page(ui, story, stale);
                });
        }
    }
    action
}

/// The toolbar: the arrow (when there is somewhere to go), the host, and the three links.
///
/// Answers the press, or `None` for a frame in which nothing was pressed — an `Option` rather
/// than `Action::None` so the caller can tell "the toolbar answered nothing" from "the toolbar
/// answered None", which is the same value the page returns but a different fact.
fn toolbar(ui: &mut egui::Ui, host: &str, can_back: bool) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        // First, because it is the one control a reader needs when the page covers the window:
        // the arrow goes back in the page's history, this leaves the page.
        if link(ui, CLOSE, "Leave this page and go back to the archive") {
            action = Some(Action::Close);
        }
        if can_back && link(ui, ARROW, "Back to the page you came from") {
            action = Some(Action::Back);
        }
        // The host is where the reader is, not who filed the story: it comes from the address
        // itself, and it stays a label — a destination a reader cannot click back to is still
        // a destination they must be able to read.
        ui.label(theme::dim(&clip(host, 48)));
        ui.add_space(theme::space::SM);
        if link(ui, RELOAD, "Read this address again") {
            action = Some(Action::Refresh);
        }
        if link(ui, ASK, "Ask the model about this story") {
            action = Some(Action::Ask);
        }
        if link(ui, TAG, "Give this story a label of your own") {
            action = Some(Action::Tag);
        }
    });
    action
}

/// The stored text as the page.
///
/// Called only in the fallback case: the engine, when there is one, paints the page and this
/// function never runs. The order is the reader's: title, where and when, the one line about
/// freshness, the words, the address.
fn text_page(ui: &mut egui::Ui, story: &Story, stale: bool) {
    let title = bounded(&story.title);
    ui.label(theme::page_title(if title.is_empty() {
        "Untitled story"
    } else {
        &title
    }));
    let by = meta(story);
    if !by.is_empty() {
        ui.add_space(2.0);
        ui.label(theme::dim(&by));
    }
    // The freshness line sits above the text, in the margin between the facts and the words:
    // it is about *this copy*, not about the story, so it belongs to neither.
    if stale {
        ui.add_space(theme::space::XS);
        ui.label(theme::dim(STALE_LINE));
    }
    ui.add_space(theme::space::SM);
    if !story.full_text {
        ui.label(theme::dim(TEASER_SENTENCE));
        ui.add_space(theme::space::XS);
    }
    let paragraphs = paragraphs(&story.text);
    if paragraphs.is_empty() {
        ui.label(theme::dim("No text was stored for this story."));
    } else {
        for paragraph in paragraphs {
            ui.label(theme::body(&paragraph));
            ui.add_space(theme::space::SM);
        }
    }
    ui.add_space(theme::space::SM);
    address(ui, story);
    ui.add_space(theme::space::SM);
    // The sentence that keeps the page honest, last, where a reader who has read the whole
    // text arrives: this is not the page, it is the archive's copy of the page's words.
    ui.label(theme::dim(ENGINE_SENTENCE));
}

/// The address line: the URL as a **copy** link, and its host said in words.
///
/// Never a launcher. `crate::link::openable` gates it — http(s) only, no credentials, printable
/// ASCII, bounded length — and an address that fails is **not repeated**: it is described.
///
/// Both halves of that rule are leaks that were measured, not imagined. The refused arm used
/// to say "Source address (not a web address, not offered): <the address>", and for
/// `https://reader:hunter2@news.example.com/story-1` that line painted the password — the same
/// password the toolbar painted a second time, since it fell back to echoing the whole address
/// as its label. So: the credential is never a destination, and only the authority (with
/// everything up to the last `@` stripped, which is what a URL parser calls user info) is ever
/// named.
fn address(ui: &mut egui::Ui, story: &Story) {
    if story.url.trim().is_empty() {
        return;
    }
    match crate::link::openable(&story.url) {
        Some(url) => {
            // The button's text is the destination, so it is bounded like every other label:
            // `openable` allows a 253-character host, and a host is stored text too.
            if ui.button(clip(&crate::link::label(url), MAX_LABEL - 1)).clicked() {
                ui.ctx().copy_text(url.to_string());
            }
            // What the button does, and what this window can do with the page when it has an
            // engine: the copy is for a browser elsewhere, and the page itself is one press
            // away when the system web view is there.
            ui.label(theme::dim(
                "Copy the address, or read the page itself in this window when the system web view is available.",
            ));
        }
        None => {
            let sentence = match web_authority(&story.url) {
                Some(authority) => format!(
                    "This address is not one the window opens: {}.",
                    clip(&authority, 80)
                ),
                None => "This address is not one the window opens.".to_string(),
            };
            ui.label(theme::dim(&sentence));
        }
    }
}

/// A toolbar control: a framed caption button.
///
/// **Framed on purpose**, the same decision `views::topics::table::request_link` records:
/// `Button::frame(false)` reads as a text link, but egui draws the keyboard's focus ring *as*
/// the frame, so a frameless button is a control a keyboard reader cannot see. The frame is
/// the theme's own hairline, not an ornament.
fn link(ui: &mut egui::Ui, label: &str, hover: &str) -> bool {
    ui.add(egui::Button::new(theme::caption(label)))
        .on_hover_text(hover)
        .clicked()
}

/// `source · when`: who filed it, and when the archive saw it.
///
/// One line, bounded: a publisher name is stored text, and the measured row carried 308
/// characters of it — a label wider than the window is not a name any more.
fn meta(story: &Story) -> String {
    let source = story.source.trim();
    let when = when(&story.published);
    let line = match (source.is_empty(), when.is_empty()) {
        (true, true) => String::new(),
        (true, false) => when,
        (false, true) => source.to_string(),
        (false, false) => format!("{source} · {when}"),
    };
    bounded(&line)
}

/// The authority of an address, with the credential form removed.
///
/// `https://user:pass@host/path` has an authority of `user:pass@host`, and everything up to
/// the **last** `@` is user info, not a destination: painting it prints a password, and a
/// password in a label is a password in the next screenshot. The last `@` is also the one a URL
/// parser splits on, so this is the same rule the engine's own loader uses rather than a second
/// opinion about where the host starts.
///
/// Answers `None` when there is no `http`/`https` scheme at all. `file:///…`, `data:…`,
/// `javascript:…` and `mailto:…` have no authority worth naming, and echoing them would print a
/// home directory, a script, or an email address; the caller says one fixed sentence instead.
fn web_authority(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Control characters are flattened first, so a stored `\r` cannot repaint the line or hide
    // the half of the authority a reader is being shown.
    let flat = one_line(authority);
    let host = match flat.rsplit_once('@') {
        Some((_, host)) => host,
        None => flat.as_str(),
    }
    .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// The toolbar's answer to "where am I": the host of the address, and nothing that could be a
/// credential.
fn host(url: &str) -> String {
    match web_authority(url) {
        Some(authority) => clip(&authority, 48),
        // A refused address is not echoed here either. The toolbar still says something true:
        // this page is showing the archive's words, and the address is not one the window
        // would go to.
        None => "no web address".to_string(),
    }
}

/// The stored text as paragraphs: blank lines separate them, control characters are flattened,
/// and an empty body is no paragraph at all (so a story with no text says so once instead of
/// drawing an empty line).
///
/// The archive's text is someone else's bytes. A line holding a control character would let a
/// stored `\r` repaint the label it sits in, and a body of 20 000 characters without a break
/// would be one enormous paragraph; both are handled here rather than trusted. The third
/// defect is the one that was measured: a single 999-character block was painted as one label,
/// so each block is cut into labels of at most [`MAX_LABEL`] characters, at spaces where there
/// is one.
fn paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .flat_map(|block| block.split("\r\n\r\n"))
        .map(|block| block.trim())
        .filter(|block| !block.is_empty())
        .map(|block| {
            block
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|block| !block.is_empty())
        .flat_map(|block| chunk(&block))
        .collect()
}

/// Cut one block of prose into labels a page can lay out: greedy, at spaces, at most `max`
/// characters each.
///
/// This is a **layout** bound and not a summary: the words are all still there, in the same
/// order, and nothing is replaced by an ellipsis. The only case where characters are dropped is
/// a single word longer than a whole label — a 400-character "word" has no space to break at,
/// and painting it would be the unbounded label this exists to prevent.
fn chunk(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in block.split_whitespace() {
        // The word, or its pieces when it is longer than a label on its own.
        let mut pieces: Vec<&str> = Vec::new();
        let mut rest = word;
        while rest.chars().count() > MAX_LABEL {
            let cut = rest
                .char_indices()
                .nth(MAX_LABEL)
                .map(|(byte, _)| byte)
                .unwrap_or(rest.len());
            pieces.push(&rest[..cut]);
            rest = &rest[cut..];
        }
        pieces.push(rest);
        for piece in pieces {
            let extra = if line.is_empty() { 0 } else { 1 };
            if line.chars().count() + extra + piece.chars().count() > MAX_LABEL {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                line.push_str(piece);
            } else {
                if extra == 1 {
                    line.push(' ');
                }
                line.push_str(piece);
            }
            // A piece that fills a label on its own closes it, so the next word starts fresh.
            if line.chars().count() >= MAX_LABEL {
                out.push(std::mem::take(&mut line));
            }
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// One line, however many the stored text has: control characters become spaces.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The most a label on this page may be, in characters, ellipsis included.
///
/// A label is one line in a window at most one screen wide. Past a few hundred characters
/// nobody is reading it any more, so a stored title, publisher or paragraph that long is a
/// hostile or a broken row, and bounding it is the page's job. The number is
/// `tests/reader_privacy.rs`'s, stated once here so a failure quotes the same value the rule
/// does.
const MAX_LABEL: usize = 200;

/// A single-line label of at most [`MAX_LABEL`] characters, ellipsis included.
///
/// One character short of the bound on purpose: `views::clip` answers up to `limit` characters
/// **plus** its `…`, so asking for `MAX_LABEL` would paint `MAX_LABEL + 1`.
fn bounded(text: &str) -> String {
    clip(&one_line(text), MAX_LABEL - 1)
}

// ---------------------------------------------------------------- the link's way upward

/// The address a "Read the source" link asked the Reader to open, remembered for the shell.
///
/// This is the page-to-shell channel the crate already uses for the open row
/// (`views::topics::open_topic`): a view cannot return a value into a caller that does not take
/// one, and threading an `&mut Option<String>` through three `show` functions would change
/// three signatures for one string. The shell reads it once per frame with [`take_source`].
pub fn remember_source(ctx: &egui::Context, url: &str) {
    // Gated on the way *in*, so no caller can put a `file:` address into the slot the shell
    // reads: the one rule for what this window will open lives in `crate::link`, and the page
    // does not keep a second copy of it.
    let Some(open) = crate::link::openable(url) else {
        return;
    };
    ctx.data_mut(|data| data.insert_temp(source_slot(), open.to_string()));
}

/// The address remembered since the last call, if any. Takes it: a door is a press, and a
/// press that stayed in the store would open the same page again on the next frame.
pub fn take_source(ctx: &egui::Context) -> Option<String> {
    ctx.data_mut(|data| data.remove_temp::<String>(source_slot()))
}

fn source_slot() -> egui::Id {
    egui::Id::new("atlas-reader-source")
}

/// One line of "Read the source", drawn where a page already draws a story: the answer to the
/// reader's question "and what does the page itself say?".
///
/// Answers whether the link was pressed. The address is the row's own, gated by
/// [`crate::link::openable`]; a story with no address the window would open has no link, and
/// nothing is drawn in that space.
pub fn source_link(ui: &mut egui::Ui, url: &str) -> bool {
    let Some(open) = crate::link::openable(url) else {
        return false;
    };
    let host = host(open);
    let label = format!("Read the source · {host}");
    if link(ui, &label, "Read this story's page in the window") {
        remember_source(ui.ctx(), open);
        return true;
    }
    false
}

/// The link in the shape a hand-painted row can host: a widget placed inside `rect`, drawn
/// last, answering whether it took a press.
///
/// `views::overview` and `views::monitors` paint their rows as galleys on one `Rect`, so a
/// link there is not a line of a vertical layout — it is a widget *over* the row. The caller
/// keeps the row's own click honest by asking [`inside`] before it opens the story, so a press
/// on the link is the link's and a press anywhere else is the row's.
pub fn source_link_in(ui: &mut egui::Ui, url: &str, rect: Rect) -> bool {
    let Some(open) = crate::link::openable(url) else {
        return false;
    };
    let pressed = ui
        .put(
            rect,
            egui::Button::new(theme::caption("Read the source")),
        )
        .on_hover_text("Read this story's page in the window")
        .clicked();
    if pressed {
        remember_source(ui.ctx(), open);
    }
    pressed
}

/// Whether the pointer is inside `rect` this frame — the test a row runs before it treats a
/// press as its own, so a link placed over the row wins the click it covers.
pub fn inside(ui: &egui::Ui, rect: Rect) -> bool {
    ui.ctx()
        .input(|input| input.pointer.interact_pos())
        .is_some_and(|at| rect.contains(at))
}

/// The width the "Read the source" widget needs at caption size, so a row can reserve the cell
/// before it paints the line under it.
pub fn source_link_width(ui: &egui::Ui) -> f32 {
    let font = theme::role::CAPTION.font();
    let galley = ui
        .ctx()
        .fonts_mut(|fonts| fonts.layout_no_wrap("Read the source".to_string(), font, theme::INK));
    // The widget's own padding on both sides plus a unit of air on the left, so the link never
    // touches the text it sits beside.
    galley.size().x + theme::space::SM * 2.0 + theme::space::XS
}

/// The house harness this page's tests are drawn with, shared with the three places that host
/// a source link (`views::articles`, `views::overview`, `views::monitors`).
///
/// `#[cfg(test)]` and not private, because those three rows live in other files and measure
/// the same two things — what a frame painted, and what a press answered. Three private
/// copies would be three ways of measuring the same site, and the copy that drifted would be
/// the one nobody rereads.
#[cfg(test)]
pub(crate) mod harness {
    use super::*;
    use egui::OutputCommand as Output;

    /// Everything one frame left on the screen: its text, where each line sits, the platform
    /// commands it asked for, and the rectangles something stroked.
    #[derive(Default)]
    pub(crate) struct Painted {
        pub(crate) text: String,
        pub(crate) labels: Vec<(String, Rect)>,
        pub(crate) commands: Vec<Output>,
        /// Every stroked rectangle: the engine's outline, when one was drawn.
        pub(crate) strokes: Vec<Rect>,
    }

    impl Painted {
        pub(crate) fn has(&self, needle: &str) -> bool {
            self.text.contains(needle)
        }

        pub(crate) fn rect_of(&self, needle: &str) -> Option<Rect> {
            self.labels
                .iter()
                .find(|(text, _)| text.contains(needle))
                .map(|(_, rect)| *rect)
        }

        pub(crate) fn at(&self, needle: &str) -> egui::Pos2 {
            self.rect_of(needle)
                .unwrap_or_else(|| panic!("nothing painted says {needle:?}; painted:\n{}", self.text))
                .center()
        }
    }

    /// A context with the theme installed, at a size a page is measured at.
    pub(crate) fn fresh() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        ctx
    }

    /// One frame: the closure draws, and the answer is what it returned plus what was painted.
    ///
    /// The deltas are dropped rather than applied: a test has no window, and letting egui
    /// apply a texture delta would have the next frame read state this one never uploaded.
    pub(crate) fn drive<R>(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        build: impl FnOnce(&mut egui::Ui) -> R,
    ) -> (R, Painted) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 620.0),
            )),
            events,
            ..Default::default()
        };
        let mut answer = None;
        let mut build = Some(build);
        let output = ctx.run_ui(input, |ui| {
            // `run_ui` takes an `FnMut`, so the once-only closure is moved out of its slot on
            // the frame that runs it: a frame runs the drawing code exactly once.
            if let Some(build) = build.take() {
                answer = Some(build(ui));
            }
        });
        let mut painted = Painted::default();
        painted.commands = output.platform_output.commands.clone();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut painted);
        }
        output.drop_without_applying_deltas();
        (
            answer.expect("the frame never ran the closure that draws"),
            painted,
        )
    }

    pub(crate) fn walk(shape: &egui::epaint::Shape, out: &mut Painted) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                out.text.push_str(text.galley.text());
                out.text.push('\n');
                let rect = Rect::from_min_size(text.pos, text.galley.size());
                out.labels.push((text.galley.text().to_owned(), rect));
            }
            egui::epaint::Shape::Rect(rect) => {
                if rect.stroke.width > 0.0 {
                    out.strokes.push(rect.rect);
                }
            }
            egui::epaint::Shape::Vec(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }

    /// The two events egui reports one press of a mouse button with.
    pub(crate) fn pointer(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
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

    /// A press and a release at `at`: every answer the frames gave, in order, and the picture
    /// the last quiet frame left.
    ///
    /// The first frame is a quiet one, because egui sizes a widget on the frame that lays it
    /// out and a press arriving before that lands on a zero-sized rect — the float the window
    /// does too, since the first frame a reader sees is never the one they click in.
    pub(crate) fn press<R>(
        ctx: &egui::Context,
        at: egui::Pos2,
        mut build: impl FnMut(&mut egui::Ui) -> R,
    ) -> (Vec<R>, Painted) {
        let mut answers = Vec::new();
        let _ = drive(ctx, Vec::new(), &mut build);
        let mut painted = Painted::default();
        for pressed in [true, false] {
            let (answer, one) = drive(ctx, pointer(at, pressed), &mut build);
            answers.push(answer);
            painted.commands.extend(one.commands);
        }
        let (answer, after) = drive(ctx, Vec::new(), &mut build);
        answers.push(answer);
        painted.commands.extend(after.commands);
        painted.text = after.text;
        painted.labels = after.labels;
        painted.strokes = after.strokes;
        (answers, painted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::OutputCommand as Output;
    use crate::views::reader::harness::{fresh, walk, Painted};

    /// A story the page can draw without an archive.
    fn story() -> Story {
        Story {
            id: 7,
            title: "Ferry traffic resumes on the northern route".to_string(),
            source: "Harbour Post".to_string(),
            published: "2024-05-01T12:54:00Z".to_string(),
            url: "https://harbour.example/news/ferry".to_string(),
            text: "The northern route reopened on Wednesday.\n\nThe first ferry sailed at six."
                .to_string(),
            full_text: true,
        }
    }

    /// Two frames, as the window draws them: the first lays the scroll area out, and the second
    /// is the one a reader sees — a ScrollArea measures its content after drawing it, so a
    /// single frame paints the part it happened to clip last.
    fn frame(
        ctx: &egui::Context,
        story: &Story,
        stale: bool,
        events: Vec<egui::Event>,
    ) -> (Action, Painted) {
        // The action is the one the *events* frame answered (a press or a release); the picture
        // is the quiet frame after it, which is what a reader is left looking at.
        let (action, first) = draw(ctx, story, stale, events);
        let (_, mut quiet) = draw(ctx, story, stale, Vec::new());
        quiet.commands.splice(0..0, first.commands);
        (action, quiet)
    }

    fn draw(
        ctx: &egui::Context,
        story: &Story,
        stale: bool,
        events: Vec<egui::Event>,
    ) -> (Action, Painted) {
        let mut action = Action::None;
        let (_, painted) = harness::drive(ctx, events, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(theme::PAGE))
                .show(ui, |ui| {
                    action = show(ui, story, None, stale);
                });
        });
        (action, painted)
    }

    /// A press and a release at `at`, the two frames egui reports a click in.
    fn click(ctx: &egui::Context, story: &Story, at: egui::Pos2) -> Action {
        // One quiet frame first: egui sizes a widget on the frame that lays it out, and a press
        // arriving before that frame lands on a zero-sized rect. (The float is what the window
        // does too — the first frame a reader sees is never the one they click in.)
        let _ = frame(ctx, story, false, Vec::new());
        let mut answered = Action::None;
        for pressed in [true, false] {
            let (action, _) = frame(
                ctx,
                story,
                false,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            if action != Action::None {
                answered = action;
            }
        }
        // The frame after the release is the one that draws the answer's effect.
        let (action, _) = frame(ctx, story, false, Vec::new());
        if action != Action::None {
            answered = action;
        }
        answered
    }

    /// Press and release at `at`, answering the picture left on screen and every platform
    /// command of the four frames — a copy is asked for on the frame the click lands, and a
    /// test should not have to know which.
    fn press_release(ctx: &egui::Context, story: &Story, at: egui::Pos2) -> Painted {
        let _ = frame(ctx, story, false, Vec::new());
        let mut painted = Painted::default();
        for pressed in [true, false] {
            let (_, one) = frame(
                ctx,
                story,
                false,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            painted.commands.extend(one.commands);
        }
        let (_, after) = frame(ctx, story, false, Vec::new());
        painted.commands.extend(after.commands);
        painted.text = after.text;
        painted.labels = after.labels;
        painted
    }

    #[test]
    fn the_text_page_paints_the_title_a_paragraph_and_the_engine_sentence() {
        let ctx = fresh();
        let (action, painted) = frame(&ctx, &story(), false, Vec::new());
        assert_eq!(action, Action::None, "a frame nobody touched answered");
        assert!(
            painted.has("Ferry traffic resumes on the northern route"),
            "the page did not paint the title:\n{}",
            painted.text
        );
        assert!(
            painted.has("The northern route reopened on Wednesday."),
            "the page did not paint the first paragraph:\n{}",
            painted.text
        );
        assert!(
            painted.has("Harbour Post ·"),
            "the page did not paint `source · when`:\n{}",
            painted.text
        );
        assert!(
            painted.has(ENGINE_SENTENCE),
            "the page did not say what this build cannot render:\n{}",
            painted.text
        );
        assert!(
            painted.has("harbour.example"),
            "the toolbar did not name the host:\n{}",
            painted.text
        );
        // The toolbar's links are framed buttons, so strokes exist; the *page* rectangle is the
        // one that would span the box, and the text page must not draw it.
        assert!(
            !painted.strokes.iter().any(|rect| rect.width() > 300.0),
            "the text page drew an engine outline: {:?}",
            painted.strokes
        );
    }

    #[test]
    fn a_teaser_says_so_and_a_whole_page_does_not() {
        let ctx = fresh();
        let mut teaser = story();
        teaser.full_text = false;
        let (_, painted) = frame(&ctx, &teaser, false, Vec::new());
        assert!(
            painted.has(TEASER_SENTENCE),
            "a teaser was painted as a whole page:\n{}",
            painted.text
        );
        let (_, whole) = frame(&ctx, &story(), false, Vec::new());
        assert!(
            !whole.has(TEASER_SENTENCE),
            "a whole page was called a teaser:\n{}",
            whole.text
        );
    }

    #[test]
    fn stale_paints_its_line_and_a_fresh_copy_does_not() {
        let ctx = fresh();
        let (_, painted) = frame(&ctx, &story(), true, Vec::new());
        assert!(
            painted.has(STALE_LINE),
            "a stale copy did not say so:\n{}",
            painted.text
        );
        let (_, fresh_copy) = frame(&ctx, &story(), false, Vec::new());
        assert!(
            !fresh_copy.has(STALE_LINE),
            "a copy the archive has not moved past called itself stale:\n{}",
            fresh_copy.text
        );
    }

    #[test]
    fn the_back_arrow_is_drawn_only_when_the_caller_has_somewhere_to_go() {
        // The toolbar is the seam: `show` passes `view.is_some()`, and the arrow's whole rule
        // is that argument. Tested here because a headless test cannot build an engine page —
        // and the arrow must not depend on anything else.
        let ctx = fresh();
        let mut with = String::new();
        let mut without = String::new();
        for can_back in [true, false] {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 620.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        toolbar(ui, "harbour.example", can_back);
                    });
                },
            );
            let mut painted = Painted::default();
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut painted);
            }
            output.drop_without_applying_deltas();
            if can_back {
                with = painted.text.clone();
            } else {
                without = painted.text.clone();
            }
        }
        assert!(
            with.contains(ARROW),
            "no back arrow where the caller had somewhere to go:\n{with}"
        );
        assert!(
            !without.contains(ARROW),
            "a back arrow with nowhere to go:\n{without}"
        );
        // …and the text page, which is the `None` case, never draws one.
        let (_, page) = frame(&ctx, &story(), false, Vec::new());
        assert!(
            !page.text.contains(ARROW),
            "the text page offered a back arrow it cannot honour:\n{}",
            page.text
        );
    }

    #[test]
    fn a_press_on_each_link_answers_its_own_action() {
        let ctx = fresh();
        let (_, painted) = frame(&ctx, &story(), false, Vec::new());
        // The way out first: a child web view covers the window and has no frame of its own,
        // so this link is the page's own door and it must answer even on the text page (where
        // there is no engine and no history, and therefore no arrow).
        let at = painted.at(CLOSE);
        assert_eq!(click(&ctx, &story(), at), Action::Close);
        let at = painted.at(RELOAD);
        assert_eq!(click(&ctx, &story(), at), Action::Refresh);
        let at = painted.at(ASK);
        assert_eq!(click(&ctx, &story(), at), Action::Ask);
        let at = painted.at(TAG);
        assert_eq!(click(&ctx, &story(), at), Action::Tag);
        // A press on a word in the text is not a control.
        let at = painted.at("The first ferry sailed at six.");
        assert_eq!(click(&ctx, &story(), at), Action::None);
    }

    #[test]
    fn the_address_is_a_copy_link_and_never_a_launcher() {
        let ctx = fresh();
        let (_, painted) = frame(&ctx, &story(), false, Vec::new());
        let at = painted.at("Copy harbour.example");
        let copied = |commands: &[Output]| {
            commands
                .iter()
                .any(|command| matches!(command, Output::CopyText(text) if text == "https://harbour.example/news/ferry"))
        };
        let clicked = press_release(&ctx, &story(), at);
        assert!(
            copied(&clicked.commands),
            "the address line did not copy the address; commands: {:?}",
            clicked.commands
        );
        // Nothing in this frame asks the platform to open anything: the page has no door to an
        // operating-system opener, which is the crate's oldest rule about addresses.
        for needle in ["OpenUrl", "open_url", "open::"] {
            // The shipping half only: the list above is *in* this file, in a test module, and
            // a scan that read it would find its own words.
            let source = include_str!("reader.rs")
                .split("#[cfg(test)]")
                .next()
                .expect("the page before its tests");
            assert!(
                !source.contains(needle),
                "the page names `{needle}`: an address is copied, never launched"
            );
        }
    }

    #[test]
    fn no_label_starts_with_a_slash_and_none_shouts() {
        let ctx = fresh();
        let (_, painted) = frame(&ctx, &story(), true, Vec::new());
        for (label, _) in &painted.labels {
            let words = label.trim();
            assert!(
                !words.starts_with('/') && !words.starts_with('#'),
                "a label is written as a route or an anchor: {label:?}"
            );
            // A label of one or two capitals is a word ("SE", "OK") or the arrow; anything
            // longer that has no lower-case letter is a shouted label.
            let letters: Vec<char> = words.chars().filter(|ch| ch.is_alphabetic()).collect();
            let shouting = letters.len() > 2 && letters.iter().all(|ch| !ch.is_lowercase());
            assert!(!shouting, "a label shouts: {label:?}");
        }
    }

    #[test]
    fn a_hostile_row_is_drawn_as_one_flat_line() {
        let ctx = fresh();
        let mut hostile = story();
        hostile.title = "Ferry\nATLAS\nMonitors".to_string();
        hostile.text = "one\r\ntwo\n\n\nthree".to_string();
        hostile.url = "javascript:alert(1)".to_string();
        let (_, painted) = frame(&ctx, &hostile, false, Vec::new());
        assert!(
            !painted.text.contains("ATLAS\nMonitors"),
            "a stored newline forged a second line:\n{}",
            painted.text
        );
        // The sentence changed with the leak fix (`address`'s refused arm no longer repeats
        // the address), so the test follows the new wording: what must stay true is that the
        // page *says* it will not open the address and never offers to copy it.
        assert!(
            painted.has("This address is not one the window opens."),
            "an address the window will not open was offered anyway:\n{}",
            painted.text
        );
        assert!(
            !painted.text.contains("alert(1)"),
            "the page echoed an address it will not open:\n{}",
            painted.text
        );
        assert!(
            !painted.text.contains("Copy"),
            "the page offered to copy an address it will not open:\n{}",
            painted.text
        );
    }

    /// A "Read the source" link is a real widget with a real answer: pressed, it remembers the
    /// address it names; an address the window will not open is not drawn and never enters the
    /// slot.
    #[test]
    fn a_source_link_answers_the_address_it_names_and_only_an_openable_one() {
        let ctx = fresh();
        // One frame of a panel holding one link, answering what the link painted and whether
        // it reported a press.
        let draw = |url: &str, events: Vec<egui::Event>| {
            let mut answered = false;
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 620.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        answered = source_link(ui, url);
                    });
                },
            );
            let mut painted = Painted::default();
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut painted);
            }
            output.drop_without_applying_deltas();
            (answered, painted)
        };
        // Nothing pressed: the link is there, and it answers nothing.
        let (answered, painted) = draw("https://harbour.example/news/ferry", Vec::new());
        assert!(!answered, "an unpressed link answered");
        assert_eq!(
            take_source(&ctx),
            None,
            "an unpressed link remembered an address"
        );
        assert!(
            painted.has("Read the source"),
            "the link does not say what it does:\n{}",
            painted.text
        );
        assert!(
            painted.has("harbour.example"),
            "the link does not name the host it opens:\n{}",
            painted.text
        );
        // The gate: an address the window will not open is not a link at all.
        let (answered, painted) = draw("file:///etc/passwd", Vec::new());
        assert!(!answered);
        assert_eq!(
            take_source(&ctx),
            None,
            "an unopenable address was remembered"
        );
        assert!(
            !painted.has("Read the source"),
            "a link was offered for an address the window will not open:\n{}",
            painted.text
        );
        // And a press, once — over the frames a mouse click takes.
        let _ = draw("https://harbour.example/news/ferry", Vec::new());
        let (_, painted) = draw("https://harbour.example/news/ferry", Vec::new());
        let at = painted.at("Read the source");
        let mut answered = false;
        for pressed in [true, false] {
            let (one, _) = draw(
                "https://harbour.example/news/ferry",
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            answered |= one;
        }
        assert!(answered, "a press on the source link answered nothing");
        assert_eq!(
            take_source(&ctx).as_deref(),
            Some("https://harbour.example/news/ferry"),
            "the link did not leave the address it names"
        );
        assert_eq!(take_source(&ctx), None, "the slot answered twice");
    }

    /// A row that hosts a link knows whether the pointer is on it, so the row's own click stays
    /// the row's where the two overlap.
    #[test]
    fn a_row_knows_whether_the_pointer_is_on_its_link() {
        let ctx = fresh();
        let link = Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 20.0));
        let pointer = |at: Option<egui::Pos2>| {
            let mut inside_link = false;
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 620.0),
                    )),
                    events: at
                        .map(|pos| vec![egui::Event::PointerMoved(pos)])
                        .unwrap_or_default(),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        inside_link = inside(ui, link);
                    });
                },
            );
            output.drop_without_applying_deltas();
            inside_link
        };
        let _ = pointer(None);
        assert!(
            pointer(Some(link.center())),
            "a pointer on the link read as off it"
        );
        assert!(
            !pointer(Some(egui::pos2(300.0, 300.0))),
            "a pointer away from the link read as on it"
        );
    }


    #[test]
    fn the_story_is_the_archive_row_field_for_field() {
        let article = crate::data::Article {
            id: 11,
            title: "A title".to_string(),
            source: "A source".to_string(),
            published: "2024-05-01T12:54:00Z".to_string(),
            summary: "The summary.".to_string(),
            content: String::new(),
            country: "SE".to_string(),
            url: "https://example.test/a".to_string(),
            full_text: false,
        };
        let story = Story::of(&article);
        assert_eq!(story.id, article.id);
        assert_eq!(story.title, article.title);
        assert_eq!(story.url, article.url);
        assert_eq!(
            story.text, article.summary,
            "an empty body must fall back to the summary, as the pane does"
        );
        assert!(!story.full_text);
    }

    #[test]
    fn the_slot_is_taken_once() {
        let ctx = fresh();
        remember_source(&ctx, "https://harbour.example/a");
        assert_eq!(
            take_source(&ctx).as_deref(),
            Some("https://harbour.example/a")
        );
        assert_eq!(take_source(&ctx), None, "the door answered twice");
    }
}
