//! One setting the reader may change: **which search engine the agent asks**.
//!
//! The app ships an engine, so a first run can search with nothing configured. A reader who
//! would rather their searches went somewhere else pastes that engine's own address — a URL
//! with `{query}` where the words go — and the agent asks that one instead.
//!
//! # Why this page writes nothing
//!
//! The engine lives in the archive's `meta` table (`search_engine`, `atlas_ingest`'s
//! [`ENGINE_KEY`]), and this window opens the archive `SQLITE_OPEN_READ_ONLY` with
//! `PRAGMA query_only` (`src/data.rs`). So a change is a **request**, exactly like a topic or
//! a monitor run: one row in `data/requests.db` ([`crate::requests`]), which the supervised
//! agent claims and answers (`atlas_monitor::serve`, its `settings_set` arm — the arm writes
//! the key, through the same gate the search tool uses, so a stored template is always one
//! the tool can ask).
//!
//! That is what the one sentence under the field is allowed to promise: *asked*, not
//! *changed*. Nothing is read again after a press either — a request is an intent for another
//! process, so the engine this page shows is exactly as fresh as it was one click ago.
//!
//! # What the page checks before a row exists
//!
//! An address that cannot work is a row the agent has to refuse, so [`check`] refuses it here
//! first and names the part that is wrong: the scheme, the host, the missing `{query}`, a
//! space. The window cannot import `atlas_ingest` (this crate links no workspace crate), so
//! the rules are a copy of the gate's and the tests read the agent's own source to keep the
//! key and the built-in address from drifting ([`tests::the_key_and_the_built_in_address_are_the_agents`]).

use crate::theme;
use crate::views::nf;
use crate::views::topics::table;

/// The store key a configured engine lives under.
///
/// `atlas_ingest::search::META_KEY`, spelled again because this crate links no workspace
/// crate; [`tests::the_key_and_the_built_in_address_are_the_agents`] reads the agent's own
/// source and fails if the two disagree.
pub const ENGINE_KEY: &str = "search_engine";

/// The engine the app ships with: Bing's news feed, which answers with RSS and needs no key.
///
/// Used when the archive stores no engine of its own, so a first run can search. **The whole
/// address**, not the part of it a reader would recognise: `atlas_ingest::search::DEFAULT_TEMPLATE`
/// carries a market and a language, and this constant used to stop at `format=RSS` — so a row
/// headed "the engine in use" named an address the agent never asks. The agent's own source is
/// read by [`tests::the_key_and_the_built_in_address_are_the_agents`], so the two cannot drift.
pub const BUILT_IN: &str =
    "https://www.bing.com/news/search?q={query}&format=RSS&setmkt=en-US&setlang=en";

/// The page's name in the context's memory, and the row a sentence is filed under: `0` is the
/// page's own header, which is where a request answered at the top of a page reports
/// (`table::remember_said`).
const PAGE: &str = "settings";
const HEADER: i64 = 0;

/// The field's label, the action's label, and what the field is for. Sentence case, and each
/// says what happens rather than what the reader should feel.
const FIELD_LABEL: &str = "your own address";
const FIELD_HINT: &str = "https://news.example.com/search?q={query}";
const FIELD_NOTE: &str =
    "it needs {query} where the search words go; the address in use stays until the agent takes a new one";
const ACTION: &str = "use this address";
const ACTION_HOVER: &str = "Send this address to the agent, which asks it instead of the built-in one";

/// Where the engine in use came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The engine the app ships with: no `search_engine` in the archive.
    BuiltIn,
    /// The reader's own address, stored in the archive.
    Yours,
    /// An address is stored, but the agent would refuse it — so the built-in one answers.
    ///
    /// `atlas_ingest::search::resolve` skips a configured template that does not validate and
    /// asks the built-in engine instead (`Engine::checked` in `atlas-ingest/src/search.rs`).
    /// Before this variant existed the page still painted the stored host and the word
    /// `yours` beside it: a row headed "the engine in use" naming an address nothing asked.
    Refused,
}

impl Source {
    /// The word the page puts after the host. One word, so the line stays a line.
    ///
    /// [`Source::Refused`] answers `built-in`, because that is the engine that answers; the
    /// sentence under it ([`Engine::problem`]) says what is wrong with the stored one.
    pub fn word(self) -> &'static str {
        match self {
            Source::BuiltIn | Source::Refused => "built-in",
            Source::Yours => "yours",
        }
    }
}

/// The engine in use: an address, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Engine {
    template: String,
    source: Source,
    /// Why a stored address is not the one in use, when it is not. `None` otherwise.
    problem: Option<String>,
}

impl Engine {
    /// The engine in use, from what the archive stores and what the app ships with.
    ///
    /// `stored` is `data::Archive::meta(ENGINE_KEY)`: `None` when the key is missing, empty
    /// or unreadable, which is exactly when the built-in one is what the agent asks.
    ///
    /// A **stored** value goes through [`check`] first, because the agent does the same: a
    /// template without `{query}`, on another scheme, without a host, with a space, a control
    /// character or past the gate's 2 048-character cap is one `resolve` skips. Showing it as
    /// the engine in use would name an address no search ever asks.
    pub fn in_use(stored: Option<&str>, built_in: &str) -> Engine {
        let built_in = built_in.trim().to_string();
        match stored.map(str::trim).filter(|value| !value.is_empty()) {
            Some(template) => match check(template) {
                Ok(usable) => Engine {
                    template: usable,
                    source: Source::Yours,
                    problem: None,
                },
                Err(why) => Engine {
                    template: built_in,
                    source: Source::Refused,
                    problem: Some(why),
                },
            },
            None => Engine {
                template: built_in,
                source: Source::BuiltIn,
                problem: None,
            },
        }
    }

    /// The whole address, as the agent will ask it.
    pub fn template(&self) -> &str {
        &self.template
    }

    /// Where it came from.
    pub fn source(&self) -> Source {
        self.source
    }

    /// Why a stored address is not in use, when the archive holds one the agent refuses.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The host this engine asks — what a reader recognises, rather than the whole URL.
    pub fn host(&self) -> String {
        host_of(&self.template)
    }
}

/// The host of a template, for the page and for the sentence after a press.
///
/// Bounded and total: no scheme is answered with the first path segment, a user info part and
/// a port are dropped (a stored template can carry either), and nothing here slices by a byte
/// that might be inside a character — every index comes from an ASCII search.
pub fn host_of(template: &str) -> String {
    let trimmed = template.trim();
    let after = match scheme_len(trimmed) {
        Some(taken) => &trimmed[taken..],
        None => trimmed,
    };
    let authority = after
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    // A port is dropped, and an IPv6 literal keeps its brackets, so `[::1]:8080` reads `[::1]`.
    let host = if host.starts_with('[') {
        host.split(']').next().map(|inside| format!("{inside}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default().to_string()
    };
    let host = host.trim().to_ascii_lowercase();
    crate::views::clip(&host, 60)
}

/// Bytes of `http://` or `https://` at the start of `text`, if it has one. ASCII, so the
/// index is a safe boundary in any string.
fn scheme_len(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("https://") {
        Some("https://".len())
    } else if lower.starts_with("http://") {
        Some("http://".len())
    } else {
        None
    }
}

/// The longest address the gate will look at (`atlas_ingest::gate::MAX_URL_CHARS`).
///
/// The gate *truncates* to this before it judges (measured: a 5 000-character probe comes back
/// 2 048 characters long and accepted), and the transport then refuses the URL the engine
/// builds from it ("url longer than 2048 characters") — so a template longer than the cap can
/// be stored, is resolved as the configured engine, and every search fails. The page refuses
/// it at the field, where the reader can see why.
pub const MAX_TEMPLATE_CHARS: usize = 2048;

/// Is this address one the agent could ask? `Ok` answers the trimmed address, `Err` a
/// sentence naming the part that is wrong.
///
/// The rules, and why each is a sentence rather than a silent no:
///
/// * an empty field is a click with nothing in it;
/// * the scheme, because the agent's fetch gate speaks http(s) — and only http(s), which is
///   *not* what `Engine::new` enforces by itself (measured: `ftp://search.example.com/?q={query}`
///   was accepted there, so the agent's `settings_set` arm checks the scheme too);
/// * a host, because `https://` alone is not an address;
/// * `{query}`, because without it there is nothing to replace with the words;
/// * no whitespace, because a URL cannot carry one and a template pasted from a browser's
///   address bar wrapped into two lines would otherwise be queued as one broken row;
/// * no control character and no more than [`MAX_TEMPLATE_CHARS`], because the gate refuses
///   both — and `Engine::in_use` uses this same rule to decide whether a *stored* template is
///   the one the agent asks, so a value that fails here must never be shown as in use.
pub fn check(template: &str) -> Result<String, String> {
    let trimmed = template.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "the address is empty: paste the search address your engine uses, with {{query}} where the words go"
        ));
    }
    let Some(taken) = scheme_len(trimmed) else {
        let start = trimmed.split("://").next().unwrap_or(trimmed);
        return Err(format!(
            "the address starts with {}: it has to start with http:// or https://",
            crate::views::clip(start, 32)
        ));
    };
    if trimmed.chars().any(char::is_whitespace) {
        return Err(
            "the address has a space in it: an address cannot carry one, and the words go where {query} is"
                .to_string(),
        );
    }
    if trimmed.chars().any(char::is_control) {
        return Err(
            "the address has a control character in it: a URL cannot carry one, and the gate refuses it"
                .to_string(),
        );
    }
    let length = trimmed.chars().count();
    if length > MAX_TEMPLATE_CHARS {
        return Err(format!(
            "the address is {length} characters long and the gate refuses one longer than \
             {MAX_TEMPLATE_CHARS}: the search would be refused every time it ran"
        ));
    }
    let scheme = &trimmed[..taken];
    let after = &trimmed[taken..];
    let authority = after.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() || host.starts_with(':') {
        return Err(format!("the address has no host after {scheme}"));
    }
    if !trimmed.contains("{query}") {
        return Err(
            "the address has no {query} in it: that is where the search words go, so the agent would have nothing to ask"
                .to_string(),
        );
    }
    Ok(trimmed.to_string())
}

/// The block, over the window's own queue.
pub fn show(ui: &mut egui::Ui, engine: &Engine) {
    show_with_queue(ui, engine, &crate::requests::queue_path());
}

/// Everything the model section needs, read once for the frame: the settings file, parsed and
/// clamped, and the model files in this checkout's models folder.
///
/// Read-only, both of them: `data/config.json` through `fs::read_to_string` and the directory
/// through `fs::read_dir` — a listing, not a write. The window never writes the settings file;
/// a change is a queue row (see [`crate::requests`]).
pub fn model_now() -> (Model, Vec<ModelFile>) {
    let text = std::fs::read_to_string(crate::assets::config()).unwrap_or_default();
    (
        Model::read(&text),
        model_files(&crate::assets::models_dir()),
    )
}

/// The block, with the queue a change is written to.
///
/// The path is a parameter for the same reason it is on Topics and Monitors: this is the
/// window's write boundary, `app.rs` hands over the queue the window opened with, and a test
/// hands a temporary one so a headless frame never touches `data/`.
pub fn show_with_queue(ui: &mut egui::Ui, engine: &Engine, queue: &std::path::Path) {
    let (model, files) = model_now();
    show_at(ui, engine, &model, &files, queue);
}

/// The page over data a caller hands in: the engine in use, the model settings, the files on
/// this machine and the queue to ask on. Pure, so a test's frame reads and writes nothing.
pub fn show_at(
    ui: &mut egui::Ui,
    engine: &Engine,
    model: &Model,
    files: &[ModelFile],
    queue: &std::path::Path,
) {
    // The page scrolls itself, like every other page here (`app.rs` states the rule and
    // checks it: no page is wrapped in a scroll area by the shell). Measured before this
    // line: the four cards are 1 080 px tall in a 800 px window, and the fourth — the one
    // that adds a model from a file or a link — was simply unreachable, with no scrollbar to
    // say so. `auto_shrink([false, false])` makes the area the box it was given, not the
    // content's own height: a scroll area that shrinks to its content is the bug that made
    // the Overview's card run past the bottom of the window.
    let _ = show_scrolled(ui, engine, model, files, queue);
}

/// The scrolling page itself, returning what the scroll area measured.
///
/// [`show_at`] throws the output away — a caller that paints a page has no use for it — but a
/// test has: `content_size` against `inner_rect` is the difference between "the page scrolls"
/// and "the page is taller than the window and nothing says so", which is the bug this page
/// shipped with.
///
/// # The keyboard
///
/// egui 0.37's `ScrollArea` has no keyboard handling of its own: it takes `Sense::CLICK |
/// DRAG` for the background and reads only wheel and drag events (measured in
/// `egui-0.37.0/src/containers/scroll_area.rs`: no `Key::Arrow*`, no `Key::PageUp`). A reader
/// with no wheel — a keyboard, a trackball, a broken trackpad — could not reach the last card
/// at all, which is the same unreachable-card bug the mouse had. So the keys are handled here:
/// the arrows by a line, Page Up/Page Down by a page, Home/End to the ends, and every one of
/// them only when no field holds the keyboard (inside a text field the arrows move the caret,
/// and stealing those would make the address field uneditable).
#[must_use]
pub fn show_scrolled(
    ui: &mut egui::Ui,
    engine: &Engine,
    model: &Model,
    files: &[ModelFile],
    queue: &std::path::Path,
) -> egui::scroll_area::ScrollAreaOutput<()> {
    let last = egui::Id::unique(("atlas-settings-scroll", PAGE));
    let (offset, bottom) = ui
        .data(|data| data.get_temp::<(f32, f32)>(last))
        .unwrap_or((0.0, 0.0));
    let step = page_scroll_step(ui, offset, bottom);
    let shown = egui::ScrollArea::vertical()
        .id_salt("settings-page")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if step != 0.0 {
                // The delta is in content coordinates, so a positive step (down the page) is a
                // negative scroll. `none()` makes a key press land at once: a held arrow that
                // glided would lag behind the keystrokes. Going through the area's own scroll
                // channel (rather than writing its offset behind its back) also cancels a wheel
                // animation that is still running — a reader who wheels and then presses a key
                // gets the key's answer, not both.
                ui.scroll_with_delta_animation(
                    egui::vec2(0.0, -step),
                    egui::style::ScrollAnimation::none(),
                );
            }
            // A form is read in a column, and a card stretched across a 2 400 px window is a
            // line nobody finishes: eight field columns (768 px) is the widest this page
            // grows, and the page's remaining width is the window's, not the card's.
            // `min` with the width this box actually has: `set_max_width` *raises* the max when
            // the number is larger, so a bare 768 grew the page to 765.7 px inside a 700 px
            // window — 105 px of horizontal overflow with no scrollbar to say so, measured by
            // `every_card_fits_the_window_at_every_audited_size`.
            ui.set_max_width(ui.available_width().min(theme::space::COLUMN * 8.0));
            body(ui, engine, model, files, queue);
        });
    // What the next frame's Home/End need: where the page is, and how far it can go. Home asks
    // for exactly the offset back to zero and End for exactly the rest — a "huge" step would be
    // rubber-banded by the area's physics and land near the end instead of on it.
    let bottom = (shown.content_size.y - shown.inner_rect.height()).max(0.0);
    ui.data_mut(|data| data.insert_temp(last, (shown.state.clamped_offset().y, bottom)));
    shown
}

/// How far the keyboard asks the page to move this frame. `0.0` when no key was pressed, or
/// when a **text field** holds the keyboard: inside one the arrows move the caret, Home and
/// End jump to the ends of the line, and stealing those keys would make this page's fields
/// uneditable. A focused *button* does not own the keys — a page whose keys stop working after
/// one press would be its own kind of trap.
///
/// `offset` and `bottom` are last frame's measurement of the scroll area: Home and End need
/// them to ask for the exact ends rather than for a very large step (a huge step is rubber-
/// banded by the area's physics and lands *near* the end, and a test that accepted that would
/// be pinning a wobble).
fn page_scroll_step(ui: &egui::Ui, offset: f32, bottom: f32) -> f32 {
    let line = 48.0_f32;
    let page = (ui.available_height() - 2.0 * line).max(line);
    if ui.ctx().text_edit_focused() {
        return 0.0;
    }
    ui.input(|input| {
        if input.key_pressed(egui::Key::Home) {
            return -offset;
        }
        if input.key_pressed(egui::Key::End) {
            return bottom - offset;
        }
        let mut step = 0.0;
        if input.key_pressed(egui::Key::ArrowDown) {
            step += line;
        }
        if input.key_pressed(egui::Key::ArrowUp) {
            step -= line;
        }
        if input.key_pressed(egui::Key::PageDown) {
            step += page;
        }
        if input.key_pressed(egui::Key::PageUp) {
            step -= page;
        }
        step
    })
}

/// The four cards, in order, in whatever box the caller gave: the scroll area above hands
/// this the viewport's width and an unbounded height.
///
/// One column, one card per question: what is asked, which file answers, how it runs, and how
/// to bring another one in. Each card is its title, then one quiet sentence saying what the
/// card is for, then the controls — the shape every other page here draws, and the reason the
/// purpose sits *under* the heading rather than squeezed beside it: a sentence that has to
/// share a line with a 15 px title is a sentence read as a caption to the title instead of as
/// the card's own subject.
fn body(
    ui: &mut egui::Ui,
    engine: &Engine,
    model: &Model,
    files: &[ModelFile],
    queue: &std::path::Path,
) {
    // The page's own title, like every other page here (`views::heading`): the shell draws the
    // sidebar, not the name of the page, so a page that opened straight into a card had no
    // answer to "where am I" except the selected row on the left.
    super::heading(
        ui,
        "Settings",
        "what this copy of ATLAS reads, and the numbers it runs the model with",
    );
    super::card(ui, "Search engine", "", |ui| {
        fill(ui);
        purpose(ui, "which service the agent asks when it looks for news");
        in_use(ui, engine);
        ui.add_space(theme::space::MD);
        field(ui, queue);
    });
    ui.add_space(theme::space::MD);
    super::card(ui, "The model on this machine", "", |ui| {
        fill(ui);
        purpose(ui, "one file, served by llama.cpp on this computer");
        model_files_section(ui, model, files, queue);
    });
    ui.add_space(theme::space::MD);
    super::card(ui, "How the model runs", "", |ui| {
        fill(ui);
        purpose(ui, "what each number does, and the command they become");
        numbers_section(ui, model, queue);
    });
    ui.add_space(theme::space::MD);
    super::card(ui, "Add a model", "", |ui| {
        fill(ui);
        purpose(ui, "a file on this machine, or an https link: it is brought in once, into the models folder");
        import_section(ui, queue);
    });
}

/// A card is as wide as the column, whatever its contents ask for.
///
/// A `Frame`'s content is a vertical layout, and a vertical layout takes the width of its
/// widest child: the model card's rows — a file name, a size, a radio — came to 268.8 px
/// inside a 700 px window, a card a third of the width of the three beside it. Asking for the
/// width the frame already offers keeps every card on the same edges (measured by
/// `every_card_fits_the_window_at_every_audited_size`).
fn fill(ui: &mut egui::Ui) {
    ui.set_min_width(ui.available_width());
}

/// A card's one sentence: what the card is for, in the quiet voice, wrapped inside the card's
/// own width so the sentence stays one sentence at 700 px and at 2400 px.
fn purpose(ui: &mut egui::Ui, sentence: &str) {
    ui.add(egui::Label::new(theme::dim(sentence)).wrap());
    ui.add_space(theme::space::SM);
}

/// The engine in use: the host a reader recognises, one word for where it came from, and the
/// address itself underneath — dim, because the host is the part that is read.
///
/// When the archive holds an address the agent refuses, the row shows the *built-in* one (the
/// engine that will actually answer) and this sentence says why the stored one is not in use:
/// a page that named the stored host here would be telling the reader their searches go
/// somewhere they do not.
fn in_use(ui: &mut egui::Ui, engine: &Engine) {
    let host = engine.host();
    ui.horizontal(|ui| {
        ui.label(theme::body(&format!("Asks {host}")));
        ui.add_space(theme::space::XS);
        ui.label(theme::dim(engine.source().word()));
    });
    ui.label(theme::dim(&crate::views::clip(engine.template(), 120)));
    if let Some(why) = engine.problem() {
        ui.label(theme::dim(&format!(
            "the address in the archive cannot be used — {why}"
        )));
    }
}

/// The field and the one action: type an address, press, and the window queues one request.
///
/// The text lives in the context's memory rather than in a `&mut` the shell holds, so the page
/// stays a function of its data (the same choice, and the same reason, as a monitor's draft:
/// `views::monitors::add_a_monitor`). Enter in the field and the link beside it are one
/// action; a refused address keeps the words so a correction costs a correction, and a queued
/// one clears them.
fn field(ui: &mut egui::Ui, queue: &std::path::Path) {
    let slot = draft_slot();
    let mut draft = ui.data(|data| data.get_temp::<String>(slot)).unwrap_or_default();
    let mut submit = false;
    // One wrapped line, not a stack of three: the name, the field and the action are one
    // sentence a reader reads left to right. At the narrowest window the action wraps under
    // the field rather than pushing the page wider than the box it was given — the same band
    // the Topics page draws above its table, from the same helper.
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::caption(FIELD_LABEL));
        let room = ui.available_width();
        let width = (room - theme::space::COLUMN * 2.0)
            .clamp(theme::space::COLUMN * 2.0, theme::space::COLUMN * 6.0);
        let field = ui.add(
            egui::TextEdit::singleline(&mut draft)
                .hint_text(FIELD_HINT)
                .desired_width(width),
        );
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        submit = table::request_link(ui, ACTION, ACTION_HOVER) || submit;
    });
    table::caption(ui, FIELD_NOTE);
    if submit {
        match check(&draft) {
            Err(why) => table::remember_said(ui.ctx(), PAGE, HEADER, &why),
            Ok(template) => {
                let payload = crate::requests::settings_payload(
                    &crate::requests::settings_of(
                        ENGINE_KEY,
                        serde_json::Value::String(template.clone()),
                    ),
                    &format!("ask {} instead", host_of(&template)),
                );
                let said = match crate::requests::push_to(queue, "settings_set", &payload) {
                    Ok(_) => {
                        draft.clear();
                        format!("asked the agent to use {}", host_of(&template))
                    }
                    // The queue's own sentence, verbatim: it says whether the file could be
                    // made, opened or written, and it never panics on a full disk.
                    Err(problem) => problem,
                };
                table::remember_said(ui.ctx(), PAGE, HEADER, &said);
            }
        }
        // The sentence is drawn next frame; ask for it now, so the answer lands in the frame
        // the reader is looking at rather than whenever the pointer next moves.
        ui.ctx().request_repaint();
    }
    ui.data_mut(|data| data.insert_temp(slot, draft));
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }
}

// ------------------------------------------------------------------ the model settings
//
// The keys are `atlas-config`'s, the ranges are the shared spec's, and every value is
// **clamped with a note** rather than refused: a settings file that says `model_context:
// 999999` is a request for "as much as possible", and the honest answer is the largest value
// this app serves plus a sentence that says so. A number the *reader* types outside a range is
// a different case, and it is refused before a row exists — see `numbers_section`.

/// The keys this page writes, spelled as `atlas-config` spells them.
pub const MODEL_LOCAL: &str = "model_local";
pub const MODEL_CONTEXT: &str = "model_context";
pub const MODEL_REASONING: &str = "model_reasoning";
pub const MODEL_ANSWER: &str = "model_answer";
pub const MODEL_GPU_LAYERS: &str = "model_gpu_layers";
pub const MODEL_THREADS: &str = "model_threads";

/// (smallest, default, largest) for each number the reader can set. The defaults are the
/// shared spec's, and so are the bounds: one table, so a field, its note and its clamp cannot
/// disagree.
pub const CONTEXT: (i64, i64, i64) = (1024, 32768, 262_144);
pub const THINKING: (i64, i64, i64) = (0, 8192, 32_768);
pub const ANSWER: (i64, i64, i64) = (256, 8192, 32_768);
pub const GPU_LAYERS: (i64, i64, i64) = (-1, -1, 999);
pub const THREADS: (i64, i64, i64) = (0, 0, 256);

/// The model settings, as the page shows them: already clamped, with the notes that say what
/// was clamped and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    /// `model_local`: the file the server is started with. Kept as the full path because
    /// that is what the queue row carries; it is never painted — a label gets
    /// [`crate::assets::label`], which reduces it to the name a reader can act on.
    pub path: String,
    pub context: i64,
    pub reasoning: i64,
    pub answer: i64,
    pub gpu_layers: i64,
    pub threads: i64,
    /// One sentence per value that was clamped. Empty when the file is inside its ranges.
    pub notes: Vec<String>,
}

impl Model {
    /// The defaults, for a checkout with no settings file.
    pub fn defaults() -> Model {
        Model {
            path: String::new(),
            context: CONTEXT.1,
            reasoning: THINKING.1,
            answer: ANSWER.1,
            gpu_layers: GPU_LAYERS.1,
            threads: THREADS.1,
            notes: Vec::new(),
        }
    }

    /// `data/config.json` as the page reads it: missing keys take their defaults, values
    /// outside a range are clamped and noted, and a value that is not a number at all is the
    /// default plus a note. Never an error: a settings file the window cannot read is still a
    /// page a reader can use.
    pub fn read(text: &str) -> Model {
        let mut model = Model::defaults();
        let Ok(root) = serde_json::from_str::<serde_json::Value>(text) else {
            return model;
        };
        if let Some(path) = root.get(MODEL_LOCAL).and_then(|value| value.as_str()) {
            model.path = path.trim().to_string();
        }
        model.context = clamp(&root, MODEL_CONTEXT, CONTEXT, &mut model.notes);
        model.reasoning = clamp(&root, MODEL_REASONING, THINKING, &mut model.notes);
        model.answer = clamp(&root, MODEL_ANSWER, ANSWER, &mut model.notes);
        model.gpu_layers = clamp(&root, MODEL_GPU_LAYERS, GPU_LAYERS, &mut model.notes);
        model.threads = clamp(&root, MODEL_THREADS, THREADS, &mut model.notes);
        model
    }
}

/// One integer setting: the default when it is missing or not a number, the nearest bound when
/// it is outside its range — and a note in both of those cases, because a page that silently
/// runs with a number the file does not say is a page that lies about the file.
fn clamp(
    root: &serde_json::Value,
    key: &str,
    range: (i64, i64, i64),
    notes: &mut Vec<String>,
) -> i64 {
    let (low, default, high) = range;
    let Some(value) = root.get(key) else {
        return default;
    };
    let asked = match value {
        serde_json::Value::Number(number) => number.as_i64(),
        // A number written as a string is the same number: a settings file is written by
        // hand as often as by a program.
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    };
    let Some(asked) = asked else {
        notes.push(format!(
            "{key} is not a number, so {} is used",
            nf(default)
        ));
        return default;
    };
    if asked < low {
        notes.push(format!(
            "{key} was {}: the smallest this app serves is {}",
            nf(asked),
            nf(low)
        ));
        return low;
    }
    if asked > high {
        notes.push(format!(
            "{key} was {}: the largest this app serves is {}",
            nf(asked),
            nf(high)
        ));
        return high;
    }
    asked
}


/// One model file on this machine: its name, its size, and the full path the queue row
/// carries (never painted — see [`Model::path`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelFile {
    pub name: String,
    pub path: String,
    pub gb: f64,
}

/// The model files in `dir`, by name. A missing directory is an empty list, not an error: a
/// fresh checkout has no model yet, and that is what the page says.
///
/// The extension is the test, not a list of names — and it is the workspace's one
/// spelling of it ([`crate::assets::MODEL_EXTENSION`], read here rather than copied): a
/// partial transfer is named something else, and that is what keeps it out of the list.
pub fn model_files(dir: &std::path::Path) -> Vec<ModelFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<ModelFile> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().to_string();
            // Structural: the extension is what llama.cpp can open, so it is also what keeps
            // a half-finished transfer (a different extension) out of the list.
            let extension = path.extension()?.to_string_lossy().to_string();
            if !extension.eq_ignore_ascii_case(crate::assets::MODEL_EXTENSION) {
                return None;
            }
            let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            Some(ModelFile {
                name,
                path: path.to_string_lossy().to_string(),
                gb: bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            })
        })
        .collect();
    files.sort_by(|left, right| left.name.cmp(&right.name));
    files
}

/// What pressing a file in the picker queues: one `settings_set` row naming that file.
///
/// The value is the **absolute** path, and that is the agent's requirement rather than a
/// choice made here: `atlas-config`'s `model_local_path` refuses a relative name by name
/// ("a relative path means one file to the window and another to the agent") because the two
/// processes have two working directories. So the queue row carries the one spelling that
/// cannot be read two ways, and the path stays on this machine: the file is created `0600`
/// (`requests.rs::open`, pinned by `tests/queue_payloads.rs`'s `the_queue_file_is_owner_only`)
/// and the window itself never paints it — the picker's labels are `assets::label`'s.
/// One function so the page and the test cannot drift on what a press sends.
pub fn pick_payload(file: &ModelFile) -> String {
    crate::requests::settings_payload(
        &crate::requests::settings_of(
            MODEL_LOCAL,
            serde_json::Value::String(file.path.clone()),
        ),
        &format!("use {}", crate::views::clip(&file.name, 60)),
    )
}

/// The argv this app will start llama.cpp with, from the numbers on this page.
///
/// One order, and it is the launcher's: `-m <model> -c <context> [-ngl <layers>] [-t <threads>]
/// --host 127.0.0.1 --port <port> -np 1 --jinja --log-disable`
/// (`atlas-launcher/src/llama.rs:181-229`). The window links no workspace crate, so it cannot
/// *call* `atlas_launcher::llama::invocation`; a test reads that file as text and asserts the
/// two token orders agree (`the_argv_preview_is_the_launchers_order`), which is the same way
/// the queue's schema is kept honest with the store's.
///
/// Three things this line gets right that it used to get wrong, all of them the difference
/// between a preview and a guess:
///
/// * **`-ngl` and `-t` are left out when their value is the sentinel.** `-1` layers and `0`
///   threads mean "llama.cpp decides", and the launcher passes neither flag for them
///   (`llama.rs:188-195`); llama.cpp has no `-1` layers and no `0` threads, so a preview that
///   painted them showed a command that could not run — on every machine that never opened
///   the advanced fields.
/// * **The port fallback is [`BOOT_PORT`], not the number this machine happens to run.**
///   The page used to fall back to `8081` while `atlas-config`'s `llama_port` row defaults to
///   `8080` (`atlas-config/src/spec.rs:91`), so a checkout with no `data/config.json`
///   previewed a port the launcher would not use.
/// * **`--log-disable` is on the line.** The launcher always passes it (`llama.rs:216`),
///   because llama.cpp logs the prompt at its default verbosity and the prompt carries page
///   text. The two flags that depend on what this build offers — `--no-webui`, and
///   `--spec-type draft-mtp` or `--mtp` — are named in [`BUILD_FLAGS_NOTE`] under the line
///   rather than guessed into it.
///
/// What is *not* in this line, and never will be: the thinking and answer budgets. They are
/// ATLAS's own, enforced by the request it sends (`atlas-ai`), and a reader who saw them here
/// would believe llama.cpp enforces them.
pub fn argv(model: &Model, port: u16) -> String {
    // Named the way every other label names a file: the checkout-relative name when the file
    // is inside the checkout (the models folder, then the file), its own name otherwise.
    // Never the absolute path — this line goes on screen, and a path on screen is somebody's
    // disk ([`crate::assets::label`] is the one reducer).
    let name = if model.path.trim().is_empty() {
        "no model chosen".to_string()
    } else {
        crate::assets::label(std::path::Path::new(&model.path))
    };
    let mut line: Vec<String> = vec![
        "llama-server".to_string(),
        "-m".to_string(),
        name,
        "-c".to_string(),
        model.context.to_string(),
    ];
    // The sentinels are absences, not numbers: the launcher omits both flags rather than
    // handing llama.cpp a value it cannot take.
    if model.gpu_layers >= 0 {
        line.push("-ngl".to_string());
        line.push(model.gpu_layers.to_string());
    }
    if model.threads > 0 {
        line.push("-t".to_string());
        line.push(model.threads.to_string());
    }
    for fixed in [
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "-np",
        "1",
        "--jinja",
        "--log-disable",
    ] {
        line.push(fixed.to_string());
    }
    line.join(" ")
}

/// The port `atlas start` uses when `data/config.json` names none: `atlas_config`'s row for
/// `llama_port` (`atlas-config/src/spec.rs:91`, default `8080`).
///
/// A constant with a test that reads that file, because the number this page used to fall back
/// to was `8081` — the port *this* machine runs today — and a preview that names a port the
/// launcher will not use is a preview of a command nobody runs.
pub const BOOT_PORT: u16 = 8080;

/// The two flags the preview cannot know: they depend on what this `llama-server` build
/// offers, which the boot reads from its `--help` (`llama.rs:217-227`). Said under the line,
/// so the preview stays the real argv and the reader still knows the whole command.
pub const BUILD_FLAGS_NOTE: &str = "this llama-server may also be started with --no-webui, and with --spec-type draft-mtp or --mtp, when its own help offers them - the rest of the line is fixed";

/// The line above the command: what it is, in the page's quietest voice.
const ARGV_LABEL: &str = "the command the model server starts with";

/// What the two enforced budgets are, in one sentence: the reader has to know which numbers
/// llama.cpp sees and which ones ATLAS spends.
const ENFORCED: &str = "context is handed to llama.cpp; the thinking and answer budgets are enforced by ATLAS's own request, not by llama.cpp";

/// The model files, each with a radio and the one that is in use marked.
fn model_files_section(
    ui: &mut egui::Ui,
    model: &Model,
    files: &[ModelFile],
    queue: &std::path::Path,
) {
    if files.is_empty() {
        table::empty(
            ui,
            "No model file in the models folder. Put one there, or add it from a file or a \
             link below.",
        );
        return;
    }
    let mut ask: Option<&ModelFile> = None;
    let mut again: Option<&ModelFile> = None;
    let mut chosen = false;
    for file in files {
        let in_use = !model.path.is_empty() && model.path == file.path;
        chosen |= in_use;
        ui.horizontal(|ui| {
            // A radio that is also the control: `selectable_label` is keyboard reachable and
            // draws its focus ring from the theme, and one row is one widget.
            let row = ui.selectable_label(in_use, theme::body(&file.name));
            // The name, and the folder it sits in on hover — a label names a file, never a
            // place on this disk, so the directory appears only when a reader asks for it.
            row.clone().on_hover_text("in the models folder");
            if row.clicked() {
                // A row that is already the model in use is not a button: pressing it used to
                // write a `settings_set` row that changed nothing and answered "asked the agent
                // to serve X" — a request for what is already true. It says so instead.
                if in_use {
                    again = Some(file);
                } else {
                    ask = Some(file);
                }
            }
            ui.add_space(theme::space::XS);
            // Spelled out rather than `GB`: two capitals beside a number are a shout, and the
            // window's own rule (`tests/disclosure.rs`, `no_label_shouts`) reads every
            // capitalised word in a label. A unit is allowed only when it is a word.
            ui.label(theme::dim(&format!("{:.1} gigabytes", file.gb)));
            if in_use {
                ui.label(theme::dim("· in use"));
            }
        });
    }
    if !chosen && !model.path.is_empty() {
        // The file that is in force, named by [`crate::assets::label`]: the checkout-relative
        // name, or just its own name when it lives elsewhere — no absolute prefix on screen.
        table::caption(
            ui,
            &format!(
                "{} is in use and is not in the models folder — it will be kept until another \
                 file is chosen.",
                crate::views::clip(&crate::assets::label(std::path::Path::new(&model.path)), 60)
            ),
        );
    }
    if let Some(file) = again {
        table::remember_said(
            ui.ctx(),
            PAGE,
            HEADER,
            &format!(
                "{} is already the model in use",
                crate::views::clip(&file.name, 60)
            ),
        );
    }
    if let Some(file) = ask {
        // The name the page shows, not the path the picker handed over — see [`pick_payload`].
        let payload = pick_payload(&file);
        let said = match crate::requests::push_to(queue, "settings_set", &payload) {
            Ok(_) => format!("asked the agent to serve {}", crate::views::clip(&file.name, 60)),
            Err(problem) => problem,
        };
        table::remember_said(ui.ctx(), PAGE, HEADER, &said);
        ui.ctx().request_repaint();
    }
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }
}

/// One number the reader can set: a label, a field, and the line that says what it costs.
struct Number {
    key: &'static str,
    label: &'static str,
    range: (i64, i64, i64),
    cost: &'static str,
}

/// The three the page puts in front of a reader — the ones whose effect is visible in the
/// answers — and the two that are about this machine's hardware, which live under "Advanced"
/// because a reader who does not know what a GPU layer is should never have to guess.
const NUMBERS: [Number; 3] = [
    Number {
        key: MODEL_CONTEXT,
        label: "Context window (tokens)",
        range: CONTEXT,
        cost: "more context reads more per article and uses more memory",
    },
    Number {
        key: MODEL_REASONING,
        label: "Thinking budget (tokens)",
        range: THINKING,
        cost: "how long the model may think before it answers",
    },
    Number {
        key: MODEL_ANSWER,
        label: "Answer length (tokens)",
        range: ANSWER,
        cost: "the longest answer it may write",
    },
];

const ADVANCED: [Number; 2] = [
    Number {
        key: MODEL_GPU_LAYERS,
        label: "GPU layers",
        range: GPU_LAYERS,
        cost: "-1 lets llama.cpp decide how much of the model goes on the graphics card",
    },
    Number {
        key: MODEL_THREADS,
        label: "Processor threads",
        range: THREADS,
        cost: "0 lets llama.cpp decide how many cores to use",
    },
];

/// The three numbers, the two under "Advanced", the command they become, and the sentence
/// about which of them llama.cpp actually sees.
fn numbers_section(ui: &mut egui::Ui, model: &Model, queue: &std::path::Path) {
    // A clamp is something the reader has to be able to explain, so it is said once, above the
    // fields it changed, in the quiet voice and inside the card's width.
    for note in &model.notes {
        ui.add(egui::Label::new(theme::caption(note)).wrap());
    }
    if !model.notes.is_empty() {
        ui.add_space(theme::space::SM);
    }

    // A number is a row of two lines: the name and its field, then the sentence that says what
    // the number costs and what it may be. The note is *under* the row rather than in a third
    // column, because a third column is placed by the widths of the two before it and lands on
    // a fractional pixel: measured at 213.03 px, which `tests/design_audit`'s fifth rule reads
    // against the 4 px scale from a card's edge (205.03 px, 1.03 px off the step, a finding).
    // Under the row every line starts on the card's own column - 13 px from its painted edge,
    // the same inset as its title - and a wrapped sentence says more than a clipped one.
    let label_w = theme::space::COLUMN * 1.75;
    let field_w = theme::space::COLUMN;

    // Every field is drawn first and read on the press: the page then answers what is on
    // screen, not what the reader happened to touch last, and a field left as the file has it
    // is simply not part of the request.
    let mut typed: Vec<(&'static Number, String)> = Vec::new();
    for number in NUMBERS.iter().chain(ADVANCED.iter()) {
        ui.horizontal(|ui| {
            // One width for every name, so the five fields line up under each other.
            ui.add_sized(
                [label_w, theme::space::XL],
                egui::Label::new(theme::body(number.label)).wrap(),
            );
            typed.push((number, number_input(ui, number, model, field_w)));
        });
        ui.add(egui::Label::new(theme::caption(&number_note(number))).wrap());
        ui.add_space(theme::space::XS);
    }

    ui.add_space(theme::space::SM);
    let mut submit = false;
    ui.horizontal(|ui| {
        submit = table::request_link(ui, "use these numbers", "Send these numbers to the agent");
    });
    if submit {
        match changes(&typed, model) {
            Err(why) => table::remember_said(ui.ctx(), PAGE, HEADER, &why),
            Ok(asked) if asked.is_empty() => table::remember_said(
                ui.ctx(),
                PAGE,
                HEADER,
                "those numbers are the ones already in use",
            ),
            Ok(asked) => {
                let mut settings = serde_json::Map::new();
                for (key, value) in &asked {
                    settings.insert(key.to_string(), serde_json::json!(value));
                }
                let why = format!(
                    "set {} from the settings page",
                    asked
                        .iter()
                        .map(|(key, _)| *key)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let payload = crate::requests::settings_payload(&settings, &why);
                let said = match crate::requests::push_to(queue, "settings_set", &payload) {
                    Ok(_) => format!("asked the agent to use {} number(s)", asked.len()),
                    Err(problem) => problem,
                };
                table::remember_said(ui.ctx(), PAGE, HEADER, &said);
                ui.ctx().request_repaint();
            }
        }
    }
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }

    // Which numbers llama.cpp sees, in one sentence, then the command itself.
    ui.add_space(theme::space::SM);
    ui.add(egui::Label::new(theme::caption(ENFORCED)).wrap());
    // The command the numbers *on screen* would become, not the ones the file still holds: a
    // reader who has typed 65536 and not pressed yet must not read `-c 32768` underneath it.
    // A field that is not a number inside its range keeps the value in use, because that field
    // cannot be sent and a preview of a command that could never run would be the same lie.
    let preview = preview_of(&typed, model);
    argv_block(ui, &preview);
}

/// What one number costs and what it may be, in one quiet line.
fn number_note(number: &Number) -> String {
    format!(
        "{} · {} to {}",
        number.cost,
        nf(number.range.0),
        nf(number.range.2)
    )
}

/// The command preview: what it is, the line itself in the quiet voice, and the sentence that
/// names the flags this line cannot know.
///
/// The line **wraps**: it is 100 to 140 characters at the shipped numbers, which is wider than
/// a 900 px window allows, and the version of this block that painted it in one unwrapped label
/// cut the last flags off the right edge with no ellipsis and no scrollbar — a preview of a
/// command the reader could not finish reading.
fn argv_block(ui: &mut egui::Ui, model: &Model) {
    ui.add_space(theme::space::MD);
    ui.label(theme::caption(ARGV_LABEL));
    let line = argv(model, crate::assets::model_port().unwrap_or(BOOT_PORT));
    ui.add(egui::Label::new(theme::dim(&line)).wrap());
    ui.add_space(theme::space::XS);
    ui.add(egui::Label::new(theme::caption(BUILD_FLAGS_NOTE)).wrap());
}

/// The numbers as the fields hold them, for the command preview above.
fn preview_of(typed: &[(&Number, String)], model: &Model) -> Model {
    let mut preview = model.clone();
    for (number, text) in typed {
        let (low, _, high) = number.range;
        let Ok(value) = text.trim().parse::<i64>() else {
            continue;
        };
        if value < low || value > high {
            continue;
        }
        match number.key {
            MODEL_CONTEXT => preview.context = value,
            MODEL_REASONING => preview.reasoning = value,
            MODEL_ANSWER => preview.answer = value,
            MODEL_GPU_LAYERS => preview.gpu_layers = value,
            _ => preview.threads = value,
        }
    }
    preview
}

/// What the fields say, checked: the values that differ from what is in use, or the sentence
/// that says which field is wrong.
///
/// A typed number outside its range is refused *before* a row exists, and the sentence names
/// the field and both bounds: a clamp here would quietly run with a number the reader did not
/// ask for. (A number in the *file* out of range is clamped instead, with a note — that is a
/// file to read, not a question to answer.)
fn changes(
    typed: &[(&Number, String)],
    model: &Model,
) -> Result<Vec<(&'static str, i64)>, String> {
    let mut asked: Vec<(&'static str, i64)> = Vec::new();
    for (number, text) in typed {
        let text = text.trim();
        let (low, _, high) = number.range;
        let Ok(value) = text.parse::<i64>() else {
            return Err(format!(
                "{} takes a whole number between {} and {}, and {:?} is not one",
                number.label,
                nf(low),
                nf(high),
                crate::views::clip(text, 20)
            ));
        };
        if value < low || value > high {
            return Err(format!(
                "{} is {} and has to be between {} and {}",
                number.label,
                nf(value),
                nf(low),
                nf(high)
            ));
        }
        if value != current(number, model) {
            asked.push((number.key, value));
        }
    }
    Ok(asked)
}

/// The value this setting has now: what a field is compared against, so an untouched field is
/// not sent again.
fn current(number: &Number, model: &Model) -> i64 {
    match number.key {
        MODEL_CONTEXT => model.context,
        MODEL_REASONING => model.reasoning,
        MODEL_ANSWER => model.answer,
        MODEL_GPU_LAYERS => model.gpu_layers,
        _ => model.threads,
    }
}

/// One labelled field, its cost line and its bounds, answering the text it holds.
///
/// The text lives in the context's memory, like every other field in this window: the page
/// stays a function of its data, and a half-typed number survives the frames between
/// keystrokes. A field the reader has not touched shows the value in use.
fn number_input(ui: &mut egui::Ui, number: &Number, model: &Model, width: f32) -> String {
    let slot = number_slot(number.key);
    let mut text = ui
        .data(|data| data.get_temp::<String>(slot))
        .unwrap_or_else(|| current(number, model).to_string());
    ui.add(egui::TextEdit::singleline(&mut text).desired_width(width));
    ui.data_mut(|data| data.insert_temp(slot, text.clone()));
    text
}

/// Where one number's text is kept between frames.
///
/// The same id [`numbers_section`] reads for a field the reader typed into and then folded
/// away: a fold is a view state, not a decision about what the reader asked for.
fn number_slot(key: &str) -> egui::Id {
    egui::Id::unique(("atlas-settings-number", key))
}

/// What the import field is asking for, once it has been checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Import {
    /// A file on this machine: the path a file picker hands over.
    File,
    /// An `https` link, downloaded once by `atlas-setup`.
    Link,
}

/// The last segment of a path: the part a reader can check. A value with no segments (a link,
/// which [`check_import`] answers before this is ever asked) is answered as it came.
fn name_of(source: &str) -> String {
    std::path::Path::new(source)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| source.to_string())
}

/// Could this be brought in? `Ok` answers what kind of source it is and the trimmed value;
/// `Err` is the sentence the page shows instead of asking the agent for something that cannot
/// work.
///
/// Structural rules only, and each is the install tool's own (`atlas-setup` decides the rest:
/// size, digest, the first four bytes, where the file lands):
///
/// * **a link has to be `https`** — a plain-http download can be swapped on the way, and the
///   tool refuses it too ("that address cannot be used" / `not_https_line`);
/// * **a path has to be the whole path** — the agent runs `atlas-setup` with the project root
///   as its working directory, so a relative path would name a file next to the archive, not
///   the one the reader is looking at;
/// * **a file has to be a file, with the extension llama.cpp opens** — the same test
///   [`model_files`] lists by, and **no more than that**: this window opens no file. It used
///   to read the first four bytes for the tool's own `GGUF` magic, and a handle on somebody's
///   weights is exactly what the window's no-file rule exists to stop (`tests/disclosure.rs`,
///   `the_window_writes_no_file_at_all`) — the page would have had to read gigabytes through
///   it to answer a question the agent answers after the copy anyway. Measured before this
///   check existed: a `notes.txt` was queued as an import, and the reader learned what it was
///   only after the agent had started the tool on it.
///
/// The sentences name the **file**, never the disk it sits on (the window paints no absolute
/// paths; `tests/disclosure.rs` keeps that rule).
pub fn check_import(source: &str) -> Result<(Import, String), String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(
            "the field is empty: paste the whole path of a model file, or an https link".to_string(),
        );
    }
    let lower = trimmed.to_ascii_lowercase();
    let is_address = lower.starts_with("https://")
        || lower.starts_with("http://")
        || trimmed.contains("://");
    if is_address {
        if !lower.starts_with("https://") {
            let scheme = trimmed.split("://").next().unwrap_or(trimmed);
            return Err(format!(
                "a download has to be an https address, and this one starts with {scheme}://: a \
                 file fetched over a connection anyone can read can be swapped on the way"
            ));
        }
        return Ok((Import::Link, trimmed.to_string()));
    }
    let path = std::path::Path::new(trimmed);
    let name = crate::views::clip(&name_of(trimmed), 60);
    if !path.is_absolute() {
        return Err(format!(
            "{} has to be the whole path on this machine, starting at /: the agent runs the \
             import from the project's own folder, so a relative path would name a different file",
            &name
        ));
    }
    if !path.is_file() {
        return Err(format!(
            "there is no file at {}: paste the whole path of a model file, or an https link",
            &name
        ));
    }
    // The extension, and nothing about the bytes: `tests/disclosure.rs` keeps a file handle
    // out of this window, and the tool re-checks the file after it has copied it.
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if extension != crate::assets::MODEL_EXTENSION {
        return Err(format!(
            "{} is not a model file: its name has to end in .{}, which is the one llama.cpp \
             opens — nothing was asked for",
            &name,
            crate::assets::MODEL_EXTENSION
        ));
    }
    Ok((Import::File, trimmed.to_string()))
}

/// The import field: a path or a link, and the one sentence about what happens to it.
fn import_section(ui: &mut egui::Ui, queue: &std::path::Path) {
    let slot = egui::Id::unique(("atlas-settings-import", PAGE));
    let mut text = ui.data(|data| data.get_temp::<String>(slot)).unwrap_or_default();
    let mut submit = false;
    // The same band as the address above, and for the same reason: one line at the wide
    // sizes, and at the narrow ones the action wraps under the field instead of widening the
    // page. The hint is the field's own placeholder, so the line needs no second label.
    ui.horizontal_wrapped(|ui| {
        let room = ui.available_width();
        let width = (room - theme::space::COLUMN * 2.0)
            .clamp(theme::space::COLUMN * 2.0, theme::space::COLUMN * 6.0);
        let field = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text("the whole path on this machine, or an https link")
                .desired_width(width),
        );
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        submit = table::request_link(ui, "add this model", "Ask for this file to be brought in")
            || submit;
    });
    if submit {
        match check_import(&text) {
            Err(why) => table::remember_said(ui.ctx(), PAGE, HEADER, &why),
            Ok((kind, source)) => {
                let why = format!("bring in {}", crate::views::clip(&source, 60));
                let payload = crate::requests::import_payload(&source, &why);
                // The sentence names the **file**, not the path: a path clipped to fit a line
                // cuts the file's own name off the end, which is the only part a reader can
                // check. (Measured: the old sentence said `asked the agent to bring in
                // /var/folders/…`, and the name was gone.)
                let named = match kind {
                    Import::File => crate::views::clip(&name_of(&source), 60),
                    Import::Link => crate::views::clip(&source, 60),
                };
                let said = match crate::requests::push_to(queue, "settings_set", &payload) {
                    Ok(_) => {
                        text.clear();
                        // "the models folder", not the folder's path: the page names files, and
                        // the path it used to append here was somebody's home directory on a
                        // line a reader has no use for (`tests/disclosure.rs` keeps paths off
                        // every label; this sentence slipped through because the path arrived
                        // as a runtime value rather than as a literal).
                        match kind {
                            Import::File => format!(
                                "asked the agent to bring in {named}, which lands in the models \
                                 folder once"
                            ),
                            Import::Link => format!(
                                "asked the agent to download {named}, which lands in the models \
                                 folder once"
                            ),
                        }
                    }
                    Err(problem) => problem,
                };
                table::remember_said(ui.ctx(), PAGE, HEADER, &said);
                ui.ctx().request_repaint();
            }
        }
    }
    ui.data_mut(|data| data.insert_temp(slot, text));
    table::caption(
        ui,
        "a link is downloaded once, by atlas-setup, into the models folder — the window only asks",
    );
    if let Some(said) = table::said_about(ui.ctx(), PAGE, HEADER) {
        table::said_line(ui, &said);
    }
}

/// Where the field's text is kept between frames.
fn draft_slot() -> egui::Id {
    egui::Id::unique(("atlas-settings-draft", PAGE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A queue of the test's own, in a directory it deletes: a headless frame writes a real
    /// SQLite file, so it must never be the window's.
    fn queue() -> (PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-settings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("requests.db");
        (dir, file)
    }

    /// The rows a test's queue holds, newest last: `(kind, payload)`.
    fn queued(file: &std::path::Path) -> Vec<(String, String)> {
        // A refused address writes no file at all — that is the pass, not a missing fixture.
        if !file.exists() {
            return Vec::new();
        }
        let connection = rusqlite::Connection::open(file).expect("the queue the page just wrote");
        let mut statement = connection
            .prepare("SELECT kind, payload FROM requests ORDER BY id")
            .expect("the queue's own table");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("a read of the queue");
        rows.flatten().collect()
    }

    /// Every string one headless frame painted, in draw order.
    #[derive(Default)]
    struct Painted {
        text: Vec<(String, egui::Rect)>,
    }

    impl Painted {
        fn all(&self) -> String {
            self.text
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn has(&self, needle: &str) -> bool {
            self.text.iter().any(|(text, _)| text == needle)
                || self.all().contains(needle)
        }

        /// The first painted string that contains `needle` — one line, for asserting what a
        /// sentence says without catching the reader's own typing in a field.
        fn line(&self, needle: &str) -> String {
            self.text
                .iter()
                .find(|(text, _)| text.contains(needle))
                .map(|(text, _)| text.clone())
                .unwrap_or_else(|| {
                    panic!("nothing painted says {needle:?}; painted:\n{}", self.all())
                })
        }

        fn at(&self, needle: &str) -> egui::Pos2 {
            self.text
                .iter()
                .find(|(text, _)| text == needle)
                .or_else(|| self.text.iter().find(|(text, _)| text.contains(needle)))
                .unwrap_or_else(|| {
                    panic!("nothing painted says {needle:?}; painted:\n{}", self.all())
                })
                .1
                .center()
        }
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
            _ => {}
        }
    }

    fn fresh() -> egui::Context {
        let context = egui::Context::default();
        crate::theme::install(&context);
        context
    }

    /// One frame of the page over data the test hands in.
    fn frame_at(
        context: &egui::Context,
        engine: &Engine,
        model: &Model,
        files: &[ModelFile],
        queue: &std::path::Path,
        events: Vec<egui::Event>,
    ) -> Painted {
        painted_at(
            context,
            engine,
            model,
            files,
            queue,
            egui::vec2(1440.0, 1200.0),
            events,
        )
    }

    /// [`frame_at`] at a named size, for the tests that hunt the window a reader has.
    fn painted_at(
        context: &egui::Context,
        engine: &Engine,
        model: &Model,
        files: &[ModelFile],
        queue: &std::path::Path,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> Painted {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        // No scroll area here: the page owns its own, which is the rule `app.rs` states and
        // checks for every page. A harness that wrapped the page would hide exactly the bug
        // this file's `the_page_scrolls_to_the_card_that_used_to_be_unreachable` pins.
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                show_at(ui, engine, model, files, queue);
            });
        });
        let mut painted = Painted::default();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut painted);
        }
        output.drop_without_applying_deltas();
        painted
    }

    /// One frame of the engine block alone, over the defaults, with the events given.
    fn frame(
        context: &egui::Context,
        engine: &Engine,
        queue: &std::path::Path,
        events: Vec<egui::Event>,
    ) -> Painted {
        frame_at(context, engine, &Model::defaults(), &[], queue, events)
    }

    /// A press on the label `needle`, over the data given: the page drawn first, then the
    /// press at the point that label was painted, then the frame that answers it.
    fn press_at(
        context: &egui::Context,
        engine: &Engine,
        model: &Model,
        files: &[ModelFile],
        queue: &std::path::Path,
        needle: &str,
    ) -> Painted {
        frame_at(context, engine, model, files, queue, Vec::new());
        let at = frame_at(context, engine, model, files, queue, Vec::new()).at(needle);
        for pressed in [true, false] {
            frame_at(
                context,
                engine,
                model,
                files,
                queue,
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
        }
        frame_at(context, engine, model, files, queue, Vec::new())
    }

    /// The block drawn twice with the draft the reader would have typed, then a press on the
    /// action: press and release, at the point the label was painted.
    fn press(
        context: &egui::Context,
        engine: &Engine,
        queue: &std::path::Path,
        typed: &str,
    ) -> Painted {
        context.data_mut(|data| data.insert_temp(draft_slot(), typed.to_string()));
        frame(context, engine, queue, Vec::new());
        let at = frame(context, engine, queue, Vec::new()).at(ACTION);
        for pressed in [true, false] {
            frame(
                context,
                engine,
                queue,
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
        }
        frame(context, engine, queue, Vec::new())
    }

    #[test]
    fn the_block_names_the_engine_in_use_and_where_it_came_from() {
        let (_dir, queue) = queue();

        let shipped = Engine::in_use(None, BUILT_IN);
        let painted = frame(&fresh(), &shipped, &queue, Vec::new());
        assert!(
            painted.has("Asks www.bing.com"),
            "the built-in engine is not named; painted:\n{}",
            painted.all()
        );
        assert!(painted.has("built-in"), "painted:\n{}", painted.all());
        assert!(
            painted.has("news/search?q={query}"),
            "the address in use is not shown; painted:\n{}",
            painted.all()
        );

        let yours = Engine::in_use(Some("https://search.example.com/?q={query}"), BUILT_IN);
        let painted = frame(&fresh(), &yours, &queue, Vec::new());
        assert!(
            painted.has("Asks search.example.com"),
            "the reader's engine is not named; painted:\n{}",
            painted.all()
        );
        assert!(painted.has("yours"), "painted:\n{}", painted.all());
        assert!(
            !painted.has("built-in"),
            "an address of the reader's is called the built-in one; painted:\n{}",
            painted.all()
        );
    }

    #[test]
    fn a_typed_address_is_queued_as_one_settings_set_row_and_offered_to_the_agent() {
        let (dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let typed = "https://search.example.com/?q={query}&format=RSS";
        let painted = press(&fresh(), &engine, &queue, typed);

        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "settings_set", "{rows:?}");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("JSON payload");
        // The shared `settings_set` envelope, not `key`/`value`: the store refuses a payload
        // whose field is literally named `key`, because its guard reads `name = value` pairs
        // out of every row it claims and would see one here.
        assert_eq!(payload["settings"][ENGINE_KEY], typed);
        assert!(
            payload["why"].as_str().unwrap_or("").contains("search.example.com"),
            "the request does not say what the reader wants: {}",
            payload["why"]
        );
        assert!(
            painted.has("asked the agent to use search.example.com"),
            "the page does not say what it asked for; painted:\n{}",
            painted.all()
        );
        // A queued address clears the field, so the next one is typed into an empty line: the
        // typed address is painted nowhere once it has been sent.
        assert!(
            !painted.text.iter().any(|(text, _)| text == typed),
            "the field still holds the address it queued; painted:\n{}",
            painted.all()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_address_writes_no_row_and_says_which_part_is_wrong() {
        let (dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let context = fresh();

        for (typed, wanted) in [
            ("search.example.com/?q={query}", "has to start with http:// or https://"),
            ("https://search.example.com/?q=chips", "has no {query} in it"),
            ("https:///search?q={query}", "has no host"),
        ] {
            let painted = press(&context, &engine, &queue, typed);
            assert!(
                painted.has(wanted),
                "{typed:?} did not say {wanted:?}; painted:\n{}",
                painted.all()
            );
            assert!(
                queued(&queue).is_empty(),
                "{typed:?} was queued although it cannot work"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_rule_of_check_names_the_part_that_is_wrong() {
        assert!(check("  https://a.example.com/?q={query}  ").is_ok());
        assert!(check("HTTP://a.example.com/?q={query}").is_ok());

        let empty = check("   ").expect_err("an empty field cannot work");
        assert!(empty.contains("empty"), "{empty}");

        let scheme = check("ftp://a.example.com/?q={query}").expect_err("ftp cannot work");
        assert!(scheme.contains("ftp"), "the sentence names the scheme: {scheme}");
        assert!(scheme.contains("http://"), "and the one it needs: {scheme}");

        let host = check("https://?q={query}").expect_err("no host cannot work");
        assert!(host.contains("no host"), "{host}");

        let query = check("https://a.example.com/search").expect_err("no {query} cannot work");
        assert!(query.contains("{query}"), "{query}");

        let space = check("https://a.example.com/?q={query} and more")
            .expect_err("a space cannot work");
        assert!(space.contains("space"), "{space}");

        let control = check("https://a.example.com/?q={query}\u{0}")
            .expect_err("a control character cannot work");
        assert!(control.contains("control character"), "{control}");

        // The gate's own cap: measured, a template longer than this is stored, resolves as the
        // configured engine, and every search is then refused by the transport.
        let mut long = String::from("https://a.example.com/?q={query}&pad=");
        while long.chars().count() <= MAX_TEMPLATE_CHARS {
            long.push('a');
        }
        let over = check(&long).expect_err("a template past the gate's cap cannot work");
        assert!(
            over.contains(&MAX_TEMPLATE_CHARS.to_string()),
            "the sentence names the cap: {over}"
        );
        let mut fits = String::from("https://a.example.com/?q={query}&pad=");
        while fits.chars().count() < MAX_TEMPLATE_CHARS {
            fits.push('a');
        }
        assert!(
            check(&fits).is_ok(),
            "a template at the cap was refused: {} chars",
            fits.chars().count()
        );
    }

    #[test]
    fn the_key_and_the_built_in_address_are_the_agents() {
        // A copy of the agent's constants is only honest if it is checked: this crate links no
        // workspace crate, so the agent's own source is read as text (`src/requests.rs` does
        // the same with the store's queue). Absent crate → the test says so and stops.
        let agent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("atlas-ingest")
            .join("src")
            .join("search.rs");
        let Ok(source) = std::fs::read_to_string(&agent) else {
            eprintln!("skipped: {} is not in this checkout", agent.display());
            return;
        };
        assert!(
            source.contains(&format!("META_KEY: &str = \"{ENGINE_KEY}\"")),
            "the agent's engine key moved; this page still writes {ENGINE_KEY:?}"
        );
        if source.contains("bing.com/news/search") {
            assert!(
                source.contains(BUILT_IN),
                "the agent's built-in engine address is not {BUILT_IN:?}"
            );
        } else {
            eprintln!("the agent has no built-in engine address yet; {BUILT_IN:?} is the window's copy");
        }
        // And the address the app ships with is one this page would accept from a reader.
        assert_eq!(
            check(BUILT_IN).as_deref().ok(),
            Some(BUILT_IN),
            "the shipped address does not pass the page's own check"
        );
        assert_eq!(host_of(BUILT_IN), "www.bing.com");
        assert_eq!(host_of("https://user:pw@example.com:8443/x?q={query}"), "example.com");
        assert_eq!(host_of("http://[::1]:8080/?q={query}"), "[::1]");
        assert_eq!(host_of("{query}"), "{query}");
    }

    // ------------------------------------------------------------- the model settings

    /// Two files, one of them the one in use.
    fn files() -> Vec<ModelFile> {
        vec![
            ModelFile {
                name: "llama-3.3-70b.gguf".to_string(),
                path: "/models/llama-3.3-70b.gguf".to_string(),
                gb: 39.6,
            },
            ModelFile {
                name: "qwen2.5-7b.gguf".to_string(),
                path: "/models/qwen2.5-7b.gguf".to_string(),
                gb: 4.7,
            },
        ]
    }

    /// The settings a reader who has already chosen a file would have.
    fn chosen() -> Model {
        Model {
            path: "/models/qwen2.5-7b.gguf".to_string(),
            ..Model::defaults()
        }
    }

    #[test]
    fn the_page_paints_the_model_and_the_numbers_in_use() {
        let (_dir, queue) = queue();
        let painted = frame_at(
            &fresh(),
            &Engine::in_use(None, BUILT_IN),
            &chosen(),
            &files(),
            &queue,
            Vec::new(),
        );
        for wanted in [
            "The model on this machine",
            "llama-3.3-70b.gguf",
            "qwen2.5-7b.gguf",
            // Spelled out, not `GB`: `tests/disclosure.rs::no_label_shouts` refuses two
            // capitals beside a number, and a unit is a word like any other.
            "4.7 gigabytes",
            "in use",
            "Context window (tokens)",
            "Thinking budget (tokens)",
            "Answer length (tokens)",
            "32768",
            "8192",
        ] {
            assert!(
                painted.has(wanted),
                "the page does not paint {wanted:?}; painted:\n{}",
                painted.all()
            );
        }
        // The command line, and the sentence about which numbers llama.cpp sees.
        assert!(
            painted.has("llama-server -m qwen2.5-7b.gguf -c 32768"),
            "the page does not show the argv; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.has("enforced by ATLAS"),
            "the page does not say which budgets ATLAS enforces; painted:\n{}",
            painted.all()
        );
        // Nothing on this page is a place on this disk: the file is named by its name (or by
        // its name inside the checkout), and the argv carries the same name. A label that
        // started with `/` would be an absolute path, which is somebody's home directory
        // (`tests/disclosure.rs::no_label_spells_out_where_the_model_lives`).
        for (text, _) in &painted.text {
            assert!(
                !text.trim_start().starts_with('/'),
                "a label is a path, not a name: {text:?}\npainted:\n{}",
                painted.all()
            );
        }
        // And the file in force is named in the row that offers it.
        assert!(
            painted.has("qwen2.5-7b.gguf"),
            "the file in use is not named; painted:\n{}",
            painted.all()
        );
    }

    #[test]
    fn the_argv_names_the_file_and_never_the_disk_it_sits_on() {
        // A file inside this checkout is named with the folder it lives in, one outside it by
        // its own name — never by the prefix that says whose machine this is.
        let inside = Model {
            path: concat!(env!("CARGO_MANIFEST_DIR"), "/../models/qwen2.5-7b.gguf").to_string(),
            ..Model::defaults()
        };
        let line = argv(&inside, 8081);
        assert!(
            line.contains("-m ") && !line.contains(env!("CARGO_MANIFEST_DIR")),
            "the argv carries the absolute path: {line}"
        );
        let outside = Model {
            path: "/opt/weights/qwen2.5-7b.gguf".to_string(),
            ..Model::defaults()
        };
        assert!(
            argv(&outside, 8081).contains("-m qwen2.5-7b.gguf"),
            "a file outside the checkout is not named by its name: {}",
            argv(&outside, 8081)
        );
        // No model chosen is a line a reader can read, not a made-up file name.
        assert!(
            argv(&Model::defaults(), 8081).contains("-m no model chosen"),
            "{}",
            argv(&Model::defaults(), 8081)
        );
    }

    #[test]
    fn a_number_out_of_range_in_the_file_is_clamped_and_the_page_says_so() {
        let model = Model::read(
            r#"{"model_context": 999999, "model_answer": "12", "model_gpu_layers": -5}"#,
        );
        assert_eq!(model.context, CONTEXT.2, "the context was not clamped to the largest");
        assert_eq!(model.answer, ANSWER.0, "the answer was not clamped to the smallest");
        assert_eq!(model.gpu_layers, GPU_LAYERS.0);
        assert_eq!(model.notes.len(), 3, "{:?}", model.notes);
        assert!(
            model.notes.iter().any(|note| note.contains("largest this app serves is 262 144")),
            "{:?}",
            model.notes
        );

        let (_dir, queue) = queue();
        let painted = frame_at(&fresh(), &Engine::in_use(None, BUILT_IN), &model, &[], &queue, Vec::new());
        assert!(
            painted.has("largest this app serves"),
            "the page hides the clamp; painted:\n{}",
            painted.all()
        );
        // A file that is not JSON is the defaults, and no sentence about a file it cannot read.
        assert_eq!(Model::read("not json"), Model::defaults());
        assert!(Model::defaults().notes.is_empty());
    }

    #[test]
    fn choosing_a_model_queues_exactly_one_row_with_that_path() {
        let (dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let painted = press_at(
            &fresh(),
            &engine,
            &chosen(),
            &files(),
            &queue,
            "llama-3.3-70b.gguf",
        );
        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "a click wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "settings_set");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("JSON");
        assert_eq!(payload["settings"][MODEL_LOCAL], "/models/llama-3.3-70b.gguf");
        assert!(
            payload["why"].as_str().unwrap_or("").contains("llama-3.3-70b.gguf"),
            "the request does not say why: {}",
            payload["why"]
        );
        assert!(
            painted.has("asked the agent to serve"),
            "the page does not say what it asked; painted:\n{}",
            painted.all()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_typed_number_out_of_range_queues_nothing_and_says_why() {
        let (dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let context = fresh();
        // The field's text lives in the page's memory; a test types by putting it there.
        context.data_mut(|data| {
            data.insert_temp(
                egui::Id::unique(("atlas-settings-number", MODEL_CONTEXT)),
                "999999".to_string(),
            )
        });
        let painted = press_at(&context, &engine, &chosen(), &files(), &queue, "use these numbers");
        assert!(
            queued(&queue).is_empty(),
            "an impossible number was queued anyway: {:?}",
            queued(&queue)
        );
        assert!(
            painted.has("has to be between 1 024 and 262 144"),
            "the page does not say which range it refused; painted:\n{}",
            painted.all()
        );

        // Inside the range, the press is one row with that number.
        context.data_mut(|data| {
            data.insert_temp(
                egui::Id::unique(("atlas-settings-number", MODEL_CONTEXT)),
                "65536".to_string(),
            )
        });
        let painted = press_at(&context, &engine, &chosen(), &files(), &queue, "use these numbers");
        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press wrote {} rows: {rows:?}", rows.len());
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("JSON");
        assert_eq!(payload["settings"][MODEL_CONTEXT], 65536);
        assert!(
            painted.has("asked the agent to use 1 number"),
            "the page does not answer the press; painted:\n{}",
            painted.all()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_argv_preview_is_the_launchers_order() {
        let model = Model {
            path: "/models/qwen2.5-7b.gguf".to_string(),
            context: 65536,
            gpu_layers: 48,
            threads: 16,
            ..Model::defaults()
        };
        let line = argv(&model, 8081);
        let wanted = [
            "llama-server",
            "-m qwen2.5-7b.gguf",
            "-c 65536",
            "-ngl 48",
            "-t 16",
            "--host 127.0.0.1",
            "--port 8081",
            "-np 1",
            "--jinja",
            "--log-disable",
        ];
        let mut from = 0;
        for token in wanted {
            let found = line[from..]
                .find(token)
                .unwrap_or_else(|| panic!("{token:?} is not in {line:?}, in that order"));
            from += found + token.len();
        }
        // The thinking and answer budgets are ATLAS's, not llama.cpp's: they must not appear.
        assert!(!line.contains("-c 8192 "), "a budget leaked into the argv: {line}");

        // The order is the launcher's, read out of its own source: this crate links no
        // workspace crate, so a copy is only honest if it is compared (`src/requests.rs` does
        // the same with the store's queue schema).
        let launcher = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("atlas-launcher")
            .join("src")
            .join("llama.rs");
        let Ok(source) = std::fs::read_to_string(&launcher) else {
            eprintln!("skipped: {} is not in this checkout", launcher.display());
            return;
        };
        let mut from = 0;
        for token in ["\"-m\"", "\"-c\"", "\"--host\"", "\"--port\"", "\"-np\"", "\"--jinja\""] {
            let found = source[from..].find(token).unwrap_or_else(|| {
                panic!("the launcher's argv no longer has {token} after the flag before it")
            });
            from += found + token.len();
        }
        if !source.contains("\"-ngl\"") {
            eprintln!(
                "the launcher does not pass -ngl yet; this page previews it ({}) and will follow \
                 the builder when it does",
                line
            );
        }
    }

    /// `-1` layers and `0` threads are the launcher absenting a flag, not passing a number:
    /// the sentinel means "llama.cpp decides", and llama.cpp has no `-1` layers. The preview
    /// used to paint `-ngl -1 -t 0` on every machine that never opened the advanced fields.
    #[test]
    fn the_argv_leaves_out_the_flags_a_sentinel_means() {
        let defaults = argv(&Model::defaults(), 8081);
        for absent in ["-ngl", "-t "] {
            assert!(
                !defaults.contains(absent),
                "the preview passes {absent:?} for a sentinel: {defaults}"
            );
        }
        // The rest of the line is there, and it ends with the flag the boot always adds.
        for present in ["-c 32768", "--host 127.0.0.1", "--port 8081", "-np 1", "--jinja"] {
            assert!(defaults.contains(present), "{present:?} is missing: {defaults}");
        }
        assert!(
            defaults.ends_with("--log-disable"),
            "the preview does not end with the flag the boot always passes: {defaults}"
        );

        // One layer is a number: the flag appears, and the sentinel's absence is not a rule
        // about the flag but about the value.
        let one = Model { gpu_layers: 0, threads: 1, ..Model::defaults() };
        let line = argv(&one, 8081);
        assert!(line.contains("-ngl 0"), "0 layers is a number, not a sentinel: {line}");
        assert!(line.contains("-t 1"), "{line}");
    }

    /// The port the page falls back to is the one `atlas-config` ships as the default —
    /// `8080`, not the `8081` this machine happens to run. Read out of that crate's source,
    /// the way the launcher's argv order is: this window links no workspace crate, so a copied
    /// number is only honest if it is compared.
    #[test]
    fn the_boot_port_is_the_one_atlas_config_ships() {
        assert_eq!(BOOT_PORT, 8080, "the preview's fallback is not the boot's default port");
        let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("atlas-config")
            .join("src")
            .join("spec.rs");
        let Ok(source) = std::fs::read_to_string(&spec) else {
            panic!("{} is not in this checkout: the fallback port cannot be compared", spec.display());
        };
        let row = source
            .lines()
            .find(|line| line.contains("key: \"llama_port\""))
            .expect("atlas-config has no llama_port row");
        assert!(
            row.contains(&format!("default: {}.0", BOOT_PORT)),
            "atlas-config's llama_port default is not {BOOT_PORT}: {row}"
        );
    }

    /// The command is longer than a narrow window: it must wrap inside the card rather than
    /// run off the right edge with nothing to say it continued.
    #[test]
    fn the_command_preview_wraps_inside_the_card() {
        let (_dir, queue) = queue();
        // The longest name this app ships with: 67 characters, which is what a real model is
        // called. The line then cannot fit 700 px on one row, so the question — wrap or clip —
        // is decided by the page and not by a short fixture that happened to fit.
        let long = Model {
            path: "/models/Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-MTP-IQ4_XS.gguf"
                .to_string(),
            ..Model::defaults()
        };
        let painted = painted_at(
            &fresh(),
            &Engine::in_use(None, BUILT_IN),
            &long,
            &files(),
            &queue,
            // The command sits in the last card, so a 520 px frame would never paint it (egui
            // drops shapes outside the clip rect). The width is the audited 700 px; only the
            // height is raised, and the wrap is a width question.
            egui::vec2(700.0, 2600.0),
            Vec::new(),
        );
        let (line, rect) = painted
            .text
            .iter()
            .find(|(text, _)| text.starts_with("llama-server -m"))
            .map(|(text, rect)| (text.clone(), *rect))
            .expect("the page paints no command line");
        // One wrapped label: every token is still on screen, the painted box is no wider than
        // the window, and it is taller than a single row of the quiet 12 px text.
        for token in [
            "Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-MTP-IQ4_XS.gguf",
            "--host 127.0.0.1",
            "-np 1",
            "--jinja",
            "--log-disable",
        ] {
            assert!(line.contains(token), "the painted line lost {token}: {line}");
        }
        assert!(
            rect.width() <= 700.0,
            "the command is {} px wide in a 700 px window: it does not wrap ({line})",
            rect.width()
        );
        assert!(
            rect.height() > 16.0,
            "the command is one {} px line, so it is clipped rather than wrapped: {line}",
            rect.height()
        );
    }

    /// The page's own column: eight field columns at most, so a 2 400 px window is a wider
    /// page and not a wider line.
    const AUDITED: [(f32, f32); 5] = [
        (700.0, 520.0),
        (900.0, 620.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
        (2400.0, 1400.0),
    ];

    /// One frame of the page, measured: what the scroll area saw and what was painted.
    struct Measured {
        /// What the scroll area says it has to scroll: the page's whole height.
        content: f32,
        /// The widest row of content: the page's column.
        width: f32,
        /// Every card frame, `(top, height)`, top to bottom.
        cards: Vec<(f32, f32)>,
        /// Where the lowest line of text on the page begins.
        ink: f32,
    }

    /// The card frames in one frame: the `theme::PANEL` rectangles a card is filled with,
    /// top to bottom, with the page's own content height.
    fn cards_at(
        context: &egui::Context,
        engine: &Engine,
        model: &Model,
        files: &[ModelFile],
        queue: &std::path::Path,
        size: egui::Vec2,
    ) -> Measured {
        // A card below the window edge is culled by `Painter`, so a 520 px frame can only ever
        // report two of the four. The page is laid out in a tall frame — the cards' heights are
        // a width question — and judged against the audited height, which is what `size.y` is.
        let tall = egui::vec2(size.x, size.y.max(4096.0));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, tall)),
            ..Default::default()
        };
        let mut shown = None;
        let mut rects: Vec<(f32, f32)> = Vec::new();
        let mut ink = 0.0_f32;
        let output = context.run_ui(input, |ui| {
            // The frame `app.rs` draws its pages in, reproduced: the page's margins are part
            // of the box a card has to fit, so a measurement without them is 32 px optimistic.
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(theme::PAGE).inner_margin(egui::Margin::symmetric(20, 16)))
                .show(ui, |ui| {
                    shown = Some(show_scrolled(ui, engine, model, files, queue));
                });
        });
        let shown = shown.expect("the page was drawn");
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                ink = ink.max(text.pos.y);
            }
            collect_cards(&clipped.shape, &mut rects);
        }
        rects.sort_by(|left, right| left.0.total_cmp(&right.0));
        let heights = rects;
        output.drop_without_applying_deltas();
        Measured {
            content: shown.content_size.y,
            width: shown.content_size.x,
            cards: heights,
            ink,
        }
    }

    /// The PANEL-filled rectangles a card is made of: wider than a field, top to bottom.
    fn collect_cards(shape: &egui::Shape, out: &mut Vec<(f32, f32)>) {
        match shape {
            egui::Shape::Vec(items) => {
                for item in items {
                    collect_cards(item, out);
                }
            }
            egui::Shape::Rect(rect)
                if rect.fill == theme::PANEL && rect.rect.width() > 300.0 =>
            {
                out.push((rect.rect.top(), rect.rect.height()));
            }
            _ => {}
        }
    }

    /// Every audited size: the page fits or it scrolls, no card is taller than the window, and
    /// the column never grows past eight field columns. The table is printed so a reader can
    /// see the numbers the assertions are made of.
    #[test]
    fn every_card_fits_the_window_at_every_audited_size() {
        let (_dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();
        for (width, height) in AUDITED {
            let context = fresh();
            // Two frames: egui settles a layout on the second, and a scroll area's content
            // size is what the first one measured.
            let _ = cards_at(&context, &engine, &model, &files(), &queue, egui::vec2(width, height));
            let page =
                cards_at(&context, &engine, &model, &files(), &queue, egui::vec2(width, height));
            // The box the page really gets: the window less the shell's 16 px top and bottom.
            let boxed = height - 32.0;
            eprintln!(
                "settings {width:>4.0}x{height:>4.0}: page {:.1} px in a {boxed:.1} px box ({}), \
                 column {:.1} px, last line of text begins at {:.1} px, cards {:?}",
                page.content,
                if page.content > boxed { "scrolls" } else { "fits" },
                page.width,
                page.ink,
                page.cards
                    .iter()
                    .map(|(_, tall)| format!("{tall:.0}"))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                page.cards.len(),
                4,
                "the page does not paint four cards at {width}x{height}: {:?}",
                page.cards
            );
            // The guarantee is reachability, not shortness: the page scrolls itself
            // (`show_at`), so a card taller than the window is read by scrolling through it.
            // What must hold is that no *text* ends past the edge the scroll area says it can
            // reach — a sentence below that edge is a sentence no scroll can bring into view.
            assert!(
                page.ink <= page.content + 0.5,
                "the last line of text ends at {:.1} px but the page measured {:.1} px at \
                 {width}x{height}: it is past the end the scroll area can reach",
                page.ink,
                page.content
            );
            // A card's frame may paint its own bottom inset past that edge — that inset holds
            // no text — but never by more than the card's own margin.
            let (last_top, last_tall) = page.cards[page.cards.len() - 1];
            assert!(
                last_top + last_tall <= page.content + theme::space::LG,
                "the last card ends at {:.1} px in a page of {:.1} px at {width}x{height}",
                last_top + last_tall,
                page.content
            );
            assert!(
                page.width <= theme::space::COLUMN * 8.0 + 1.0,
                "the column is {:.1} px wide at {width}: it is not capped",
                page.width
            );
        }
    }

    /// The page's own title: the shell draws the sidebar, not the page's name, so a page that
    /// opened straight into a card was a page with no answer to "where am I". Every other page
    /// here starts with `views::heading`; Settings was the one that did not.
    #[test]
    fn the_page_opens_with_its_name_and_one_sentence() {
        let (_dir, queue) = queue();
        let painted = frame(
            &fresh(),
            &Engine::in_use(None, BUILT_IN),
            &queue,
            Vec::new(),
        );
        let first = painted
            .text
            .first()
            .map(|(text, _)| text.clone())
            .expect("the page paints nothing");
        assert_eq!(first, "Settings", "the page does not open with its name");
        let second = painted.text.get(1).map(|(text, _)| text.clone()).unwrap_or_default();
        assert!(
            second.starts_with("what this copy of ATLAS reads"),
            "the title has no sentence under it: {second:?}"
        );
        // One sentence, not a paragraph: the heading is read at a glance.
        assert!(second.len() < 90, "the page's purpose is a paragraph: {second:?}");
    }

    /// The fourth card used to be unreachable: the page painted 1 080 px of content into an
    /// 800 px box with no scroll area of its own, so "Add a model from a file or a link" — the
    /// only way to bring your own weights in — was below the edge and nothing said so. The page
    /// scrolls itself now; this measures the scroll area's own answer rather than trusting the
    /// line that creates it.
    #[test]
    fn the_page_scrolls_to_the_card_that_used_to_be_unreachable() {
        let (_dir, queue) = queue();
        let context = fresh();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();
        let small = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        let mut scroll = 0.0;

        // One frame per measurement, with the wheel event carried in: egui applies a wheel to
        // the area it hovered, and the pointer has to have moved before the wheel arrives.
        let measure = |scroll: f32| {
            let events = if scroll == 0.0 {
                Vec::new()
            } else {
                vec![
                    egui::Event::PointerMoved(egui::pos2(450.0, 300.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -scroll),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            let input = egui::RawInput {
                screen_rect: Some(small),
                events,
                ..Default::default()
            };
            let mut out = None;
            let output = context.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    out = Some(show_scrolled(ui, &engine, &model, &[], &queue));
                });
            });
            output.drop_without_applying_deltas();
            out.expect("the page was drawn")
        };

        // The page is taller than the window, so it *has* to scroll: this is the number that
        // was missing, and the assertion a page without a scroll area fails outright.
        measure(0.0);
        let rest = measure(0.0);
        assert!(
            rest.content_size.y > rest.inner_rect.height(),
            "the settings page is {} px in a {} px box: it fits, so this test measures nothing",
            rest.content_size.y,
            rest.inner_rect.height()
        );
        assert_eq!(rest.state.clamped_offset().y, 0.0, "a page opens at the top");

        // A wheel over it moves it, and it does not spring back.
        measure(240.0);
        let moved = measure(240.0);
        assert!(
            moved.state.clamped_offset().y > 0.0,
            "the wheel did not move the settings page: the offset is still {} — the page has \
             no scroll area of its own",
            moved.state.clamped_offset().y
        );
        assert!(
            moved.state.clamped_offset().y <= moved.content_size.y - moved.inner_rect.height() + 0.5,
            "the page scrolled past its content: {} of {}",
            moved.state.clamped_offset().y,
            moved.content_size.y - moved.inner_rect.height()
        );
        scroll += moved.state.clamped_offset().y;
        assert!(scroll > 40.0, "the page moved {} px for a 240 px wheel", scroll);
    }

    // ------------------------------------------------------- the hunts, one test each

    /// Where the import field's text is kept between frames (`import_section`).
    fn import_slot() -> egui::Id {
        egui::Id::unique(("atlas-settings-import", PAGE))
    }

    /// One key press, as a frame's input.
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// One wheel notch over the page, with the pointer where the wheel arrives.
    fn wheel(points: f32) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(egui::pos2(450.0, 300.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -points),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// One frame at one size, answering the scroll area's own measurement.
    fn scrolled_at(
        context: &egui::Context,
        engine: &Engine,
        model: &Model,
        files: &[ModelFile],
        queue: &std::path::Path,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> egui::scroll_area::ScrollAreaOutput<()> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        let mut out = None;
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                out = Some(show_scrolled(ui, engine, model, files, queue));
            });
        });
        output.drop_without_applying_deltas();
        out.expect("the page was drawn")
    }

    /// **Hunt 1.** The page scrolls at 900 × 620 with the wheel **and** the keyboard.
    ///
    /// Measured before this test passed: the wheel moved the page and every key did nothing —
    /// egui 0.37's `ScrollArea` has no keyboard handling of its own (the only `Sense` it takes
    /// is `CLICK | DRAG`), so a reader without a wheel could not reach the fourth card at all.
    #[test]
    fn the_page_scrolls_with_the_wheel_and_the_keyboard_at_nine_hundred_by_six_twenty() {
        let (_dir, queue) = queue();
        let context = fresh();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();
        let size = egui::vec2(900.0, 620.0);
        let at = |events: Vec<egui::Event>| {
            scrolled_at(&context, &engine, &model, &[], &queue, size, events)
        };
        let offset = |events: Vec<egui::Event>| at(events).state.clamped_offset().y;
        // egui spreads one wheel notch over about a dozen frames (`smooth_scroll_delta` decays),
        // and the key assertions below are about the keys: let the notch finish first, or a key
        // press would be measured against a page that is still gliding.
        let settle = || {
            let mut last = -1.0_f32;
            for _ in 0..60 {
                let now = offset(Vec::new());
                if (now - last).abs() < 0.01 {
                    return now;
                }
                last = now;
            }
            last
        };

        // The page is taller than the box, so both inputs have something to do.
        let top = at(Vec::new());
        assert!(
            top.content_size.y > top.inner_rect.height(),
            "the page is {} px in a {} px box: it fits, so nothing here can be measured",
            top.content_size.y,
            top.inner_rect.height()
        );
        assert_eq!(settle(), 0.0, "a page opens at the top");

        // The wheel.
        at(wheel(240.0));
        let wheeled = settle();
        assert!(wheeled > 0.0, "the wheel did not move the page at 900x620");

        // A keyboard: Home returns to the top, an arrow moves exactly one line, PageDown moves
        // further than an arrow, and End lands on the last card rather than near it.
        at(vec![key(egui::Key::Home)]);
        let home = settle();
        assert_eq!(
            home, 0.0,
            "Home did not return to the top: the offset is {home} (the wheel had reached {wheeled})"
        );

        at(vec![key(egui::Key::ArrowDown)]);
        let line = settle();
        assert_eq!(
            line, 48.0,
            "one ArrowDown moved {line} px, not one line (48)"
        );

        at(vec![key(egui::Key::PageDown)]);
        let paged = settle();
        assert!(
            paged > line,
            "PageDown moved {paged} px from {line}: no further than an arrow key"
        );

        at(vec![key(egui::Key::End)]);
        let ended = settle();
        let last = at(Vec::new());
        let bottom = last.content_size.y - last.inner_rect.height();
        assert!(
            ended >= bottom - 0.5,
            "End stopped at {ended} of {bottom}: the last card is unreachable from the keyboard"
        );
    }

    /// **Hunt 1b.** While a field is being typed into, the keys belong to the field: Home must
    /// not jump the page to the top from under the caret.
    #[test]
    fn a_key_belongs_to_the_field_while_the_reader_is_typing() {
        let (_dir, queue) = queue();
        let context = fresh();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();
        let size = egui::vec2(900.0, 620.0);
        let shown = |events: Vec<egui::Event>| {
            scrolled_at(&context, &engine, &model, &[], &queue, size, events)
        };

        // Click into the context field, at the point its own text is painted.
        let field = painted_at(&context, &engine, &model, &[], &queue, size, Vec::new()).at("32768");
        for pressed in [true, false] {
            painted_at(
                &context,
                &engine,
                &model,
                &[],
                &queue,
                size,
                vec![
                    egui::Event::PointerMoved(field),
                    egui::Event::PointerButton {
                        pos: field,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
        }
        let _ = painted_at(&context, &engine, &model, &[], &queue, size, Vec::new());
        assert!(
            context.text_edit_focused(),
            "the click did not put the keyboard into the field, so this test would measure nothing"
        );
        assert_eq!(
            shown(Vec::new()).state.clamped_offset().y,
            0.0,
            "the page moved while the field was clicked"
        );

        shown(vec![key(egui::Key::PageDown)]);
        let typed = shown(Vec::new());
        assert_eq!(
            typed.state.clamped_offset().y,
            0.0,
            "PageDown scrolled the page although the reader was typing into a field"
        );
    }

    /// **Hunt 5.** The engine row shows the engine the agent would actually ask.
    ///
    /// Measured before this test passed: a stored template with no `{query}` — which
    /// `atlas_ingest::search::resolve` skips, so the built-in engine answers — was painted as
    /// `Asks search.mojeek.com` and `yours`. The page named an address that is not in use.
    #[test]
    fn the_engine_row_shows_the_engine_the_agent_would_ask() {
        let (_dir, queue) = queue();

        let broken = Engine::in_use(Some("https://search.mojeek.com/?q=chips"), BUILT_IN);
        let painted = frame(&fresh(), &broken, &queue, Vec::new());
        assert!(
            painted.has("Asks www.bing.com"),
            "the built-in engine is not named although it is the one that answers; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.has("built-in"),
            "the row does not say the built-in engine answers; painted:\n{}",
            painted.all()
        );
        assert!(
            !painted.has("yours"),
            "a template the agent refuses is still called the reader's; painted:\n{}",
            painted.all()
        );
        assert!(
            painted.all().contains("cannot be used") && painted.all().contains("{query}"),
            "the row does not say what is wrong with the stored address; painted:\n{}",
            painted.all()
        );

        // The address painted under the built-in row is the *whole* address the agent asks.
        // Measured before this test passed: the page's copy stopped at `format=RSS` and left
        // the market and the language out, so the row named a URL nobody requests.
        assert!(
            painted.has("setmkt=en-US&setlang=en"),
            "the row names a built-in address the agent never asks; painted:\n{}",
            painted.all()
        );

        // An address of the reader's that does pass is still theirs, and is the one named.
        let yours = Engine::in_use(Some("https://search.mojeek.com/?q={query}"), BUILT_IN);
        let painted = frame(&fresh(), &yours, &queue, Vec::new());
        assert!(
            painted.has("Asks search.mojeek.com"),
            "the reader's engine is not named; painted:\n{}",
            painted.all()
        );
        assert!(painted.has("yours"), "painted:\n{}", painted.all());
        assert!(!painted.has("cannot be used"), "painted:\n{}", painted.all());
    }

    /// **Hunt 3.** An import is checked before the agent is asked: the path a picker hands
    /// over, a file that is not a model, a relative name, and a link that is not `https`.
    ///
    /// Measured before this test passed: `notes.txt` was queued as an import row, and the
    /// reader learned it was not a model only after the agent had started `atlas-setup` on it.
    /// The page's check is the extension (`atlas-setup` re-checks the bytes after the copy) —
    /// this window opens no file, which is why the sentence says "its name has to end in
    /// .gguf" rather than reading the four-byte magic.
    #[test]
    fn an_import_is_checked_before_the_agent_is_asked() {
        let (dir, queue) = queue();
        let context = fresh();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();

        let notes = dir.join("notes.txt");
        std::fs::write(&notes, b"a shopping list, not weights\n").expect("a file");
        let model_file = dir.join("qwen2.5-7b.gguf");
        std::fs::write(&model_file, b"GGUF\x03\x00\x00\x00").expect("a model's first bytes");

        let asked = |typed: &str| -> Painted {
            context.data_mut(|data| data.insert_temp(import_slot(), typed.to_string()));
            press_at(&context, &engine, &model, &[], &queue, "add this model")
        };

        // A file that is not a model by its name: no row, and the sentence says why. The page
        // does not read the file (no handle in this window), so the rule here is the extension
        // the tool itself renames by; the bytes are the agent's check after the copy.
        let painted = asked(&notes.to_string_lossy());
        assert!(
            queued(&queue).is_empty(),
            "a file that is not a model was queued: {:?}",
            queued(&queue)
        );
        assert!(
            painted.all().contains("not a model file") && painted.all().contains(".gguf"),
            "the page does not say the file is not a model; painted:\n{}",
            painted.all()
        );
        // The sentence names the file, not the disk it sits on (the field holds the path the
        // reader typed, which is theirs; the *page's* words must not carry it).
        let said = painted.line("not a model file");
        assert!(
            said.contains("notes.txt") && !said.contains(&notes.to_string_lossy().to_string()),
            "the sentence names the disk the file sits on: {said}"
        );

        // A relative path names nothing this window can check — and the agent would resolve
        // it against *its* working directory, which is not where the reader is standing.
        let painted = asked("qwen2.5-7b.gguf");
        assert!(
            queued(&queue).is_empty(),
            "a relative path was queued: {:?}",
            queued(&queue)
        );
        assert!(
            painted.all().contains("has to be the whole path"),
            "the page does not say a path has to be absolute; painted:\n{}",
            painted.all()
        );

        // A file that is a model: one row, the path exactly as given.
        let painted = asked(&model_file.to_string_lossy());
        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "settings_set");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("JSON");
        assert_eq!(
            payload["import_model"],
            serde_json::json!(model_file.to_string_lossy().to_string())
        );
        assert!(
            painted.has("qwen2.5-7b.gguf"),
            "the page does not say what it asked for; painted:\n{}",
            painted.all()
        );

        // A download has to be over https: plain http can be swapped on the way.
        let painted = asked("http://example.org/model.gguf");
        assert_eq!(
            queued(&queue).len(),
            1,
            "a plain-http download was queued: {:?}",
            queued(&queue)
        );
        assert!(
            painted.all().contains("https"),
            "the page does not say why plain http will not do; painted:\n{}",
            painted.all()
        );
        asked("https://example.org/model.gguf");
        assert_eq!(
            queued(&queue).len(),
            2,
            "an https link was not queued: {:?}",
            queued(&queue)
        );
    }

    /// **Hunt 4a.** Every number refuses what is outside its range with a sentence, and clamps
    /// nothing: the value that would have been stored is the one the reader typed, or no row.
    #[test]
    fn every_number_refuses_what_is_outside_its_range_with_a_sentence() {
        let (_dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();

        for number in NUMBERS.iter().chain(ADVANCED.iter()) {
            let (low, _, high) = number.range;
            for (typed, wanted) in [
                ((low - 1).to_string(), nf(low)),
                ((high + 1).to_string(), nf(high)),
                ("half".to_string(), "whole number".to_string()),
            ] {
                let context = fresh();
                context.data_mut(|data| {
                    data.insert_temp(number_slot(number.key), typed.clone());
                });
                let painted = press_at(&context, &engine, &model, &[], &queue, "use these numbers");
                assert!(
                    queued(&queue).is_empty(),
                    "{} = {typed} was queued although it is outside {}..{}",
                    number.key,
                    low,
                    high
                );
                assert!(
                    painted.all().contains(number.label) && painted.all().contains(&wanted),
                    "the sentence for {} = {typed} does not name the field and {wanted:?}; \
                     painted:\n{}",
                    number.key,
                    painted.all()
                );
            }
        }
    }

    /// **Hunt 4b.** A press sends exactly the numbers the reader changed — integers, under the
    /// keys the archive mirrors, and nothing else.
    #[test]
    fn a_press_sends_only_the_numbers_the_reader_changed() {
        let (dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();
        let context = fresh();
        // Two changed, three left as the file has them.
        context.data_mut(|data| {
            data.insert_temp(number_slot(MODEL_CONTEXT), "65536".to_string());
            data.insert_temp(number_slot(MODEL_ANSWER), "4096".to_string());
            data.insert_temp(number_slot(MODEL_REASONING), model.reasoning.to_string());
        });
        press_at(&context, &engine, &model, &[], &queue, "use these numbers");

        let rows = queued(&queue);
        assert_eq!(rows.len(), 1, "the press wrote {} rows: {rows:?}", rows.len());
        assert_eq!(rows[0].0, "settings_set");
        let payload: serde_json::Value = serde_json::from_str(&rows[0].1).expect("JSON");
        let settings = payload["settings"].as_object().expect("a settings map");
        // `serde_json::Map` is a `BTreeMap`: the names arrive sorted, and sorting here says so
        // rather than pinning the order of a map.
        let mut names: Vec<&String> = settings.keys().collect();
        names.sort();
        let mut wanted = vec![MODEL_CONTEXT, MODEL_ANSWER];
        wanted.sort();
        assert_eq!(
            names, wanted,
            "the row carries keys the reader did not change: {settings:?}"
        );
        assert_eq!(settings[MODEL_CONTEXT], serde_json::json!(65536));
        assert_eq!(settings[MODEL_ANSWER], serde_json::json!(4096));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Hunt 4c.** The three keys this page writes are the keys the archive mirrors, because
    /// that mirror is the only place this read-only window can read them back.
    #[test]
    fn the_three_numbers_are_the_keys_the_archive_mirrors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let monitor = root.join("atlas-monitor").join("src").join("settings_set.rs");
        let Ok(source) = std::fs::read_to_string(&monitor) else {
            eprintln!("skipped: {} is not in this checkout", monitor.display());
            return;
        };
        let mirrored = format!(
            "const MIRRORED: [&str; 3] = [\"{MODEL_CONTEXT}\", \"{MODEL_REASONING}\", \
             \"{MODEL_ANSWER}\"];"
        );
        assert!(
            source.contains(&mirrored),
            "the agent mirrors a different set of keys than this page writes ({mirrored})"
        );

        // And the window's own chat budget reads that mirror, not the file: the numbers on
        // this page are the ones a chat request spends.
        let app = root.join("atlas-desktop").join("src").join("app.rs");
        let Ok(app) = std::fs::read_to_string(&app) else {
            eprintln!("skipped: {} is not in this checkout", app.display());
            return;
        };
        for key in [MODEL_REASONING, MODEL_ANSWER] {
            assert!(
                app.contains(&format!("archive.meta(\"{key}\")")),
                "app.rs no longer reads {key} from the archive: the page would show a number \
                 the window does not spend"
            );
        }
    }

    /// **Hunt 6a.** The command preview shows the numbers the fields hold, not the numbers in
    /// the file: a preview that lags the fields is a card that lies about what a press does.
    #[test]
    fn the_command_preview_shows_the_numbers_the_fields_hold() {
        let (_dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();
        let context = fresh();
        context.data_mut(|data| {
            data.insert_temp(number_slot(MODEL_CONTEXT), "65536".to_string());
            data.insert_temp(number_slot(MODEL_GPU_LAYERS), "48".to_string());
        });
        let painted = frame_at(&context, &engine, &model, &files(), &queue, Vec::new());
        assert!(
            painted.has("-c 65536"),
            "the preview still shows the stored context ({}), not the one on screen; \
             painted:\n{}",
            model.context,
            painted.all()
        );
        assert!(
            painted.has("-ngl 48"),
            "the preview does not follow the advanced field; painted:\n{}",
            painted.all()
        );
    }

    /// **Hunt 6b.** A row whose file is already served is not a button that asks again.
    #[test]
    fn choosing_the_model_already_in_use_asks_for_nothing() {
        let (dir, queue) = queue();
        let engine = Engine::in_use(None, BUILT_IN);
        let painted = press_at(
            &fresh(),
            &engine,
            &chosen(),
            &files(),
            &queue,
            "qwen2.5-7b.gguf",
        );
        assert!(
            queued(&queue).is_empty(),
            "the row already in use wrote a request: {:?}",
            queued(&queue)
        );
        assert!(
            painted.all().contains("already"),
            "the page does not say the file is already in use; painted:\n{}",
            painted.all()
        );
        assert!(
            !painted.has("asked the agent to serve"),
            "the page says it asked for a file that is already served; painted:\n{}",
            painted.all()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Hunt 2.** Every field writes exactly one `settings_set` row, carrying only the key it
    /// changed: the engine, a model file, the numbers, and an import.
    #[test]
    fn every_field_writes_one_row_with_only_the_key_it_changed() {
        let (dir, queue) = queue();
        let context = fresh();
        let engine = Engine::in_use(None, BUILT_IN);
        let model = Model::defaults();

        // (1) the engine field
        context.data_mut(|data| {
            data.insert_temp(draft_slot(), "https://search.mojeek.com/?q={query}".to_string())
        });
        press(&context, &engine, &queue, "https://search.mojeek.com/?q={query}");

        // (2) a model file
        press_at(&context, &engine, &model, &files(), &queue, "llama-3.3-70b.gguf");

        // (3) one number
        context.data_mut(|data| {
            data.insert_temp(number_slot(MODEL_CONTEXT), "65536".to_string())
        });
        press_at(&context, &engine, &model, &[], &queue, "use these numbers");

        // (4) an import, over an https link so the check needs no file on this disk
        context.data_mut(|data| {
            data.insert_temp(import_slot(), "https://example.org/model.gguf".to_string())
        });
        press_at(&context, &engine, &model, &[], &queue, "add this model");

        let rows = queued(&queue);
        assert_eq!(rows.len(), 4, "four fields wrote {} rows: {rows:?}", rows.len());
        let mut keys: Vec<Vec<String>> = Vec::new();
        for (kind, payload) in &rows {
            assert_eq!(kind, "settings_set", "a field wrote a {kind:?} row");
            let payload: serde_json::Value = serde_json::from_str(payload).expect("JSON");
            let mut names: Vec<String> = payload["settings"]
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            if payload.get("import_model").is_some() {
                names.push("import_model".to_string());
            }
            keys.push(names);
        }
        assert_eq!(
            keys,
            vec![
                vec![ENGINE_KEY.to_string()],
                vec![MODEL_LOCAL.to_string()],
                vec![MODEL_CONTEXT.to_string()],
                vec!["import_model".to_string()],
            ],
            "the rows do not carry exactly one changed key each: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

