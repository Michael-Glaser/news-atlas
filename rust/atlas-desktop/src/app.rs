//! The frame: a rail, a page, a status row.
//!
//! One window, one archive connection, and **a frame that reads nothing**. The archive
//! is read once per *demand* — the window (24 h / 3 d / 7 d), the page, and the search
//! box — and the answer is kept in [`Atlas`], so a page is drawn from memory, the graph
//! settles at screen rate, and switching a tab costs one read instead of one per frame.
//!
//! Two things make that checkable rather than claimed: the status row prints what the
//! last read cost and how many reads there have been, and the read *decision* is a
//! value ([`Demand`] against `loaded_for`), so it is asked in the tests rather than
//! trusted. Before this change every frame cloned the map pins, the country counts and up
//! to 400 stories into a `Snapshot`, and filled the graph from cloned entity and edge rows:
//! every repaint paid for copies of data that had not changed.
//!
//! What this module does not decide: what a page looks like (the `views` modules), how
//! a query is written ([`crate::data`]), or the palette ([`crate::theme`]). It decides
//! *when* the archive is read and who gets a reference to the result.

use std::path::PathBuf;
use std::time::{Duration, Instant};

// The window's own close command is `egui::ViewportCommand::Close`, which the security spec
// reads as a *process* command: its rule is "an alias is the same call", and a line of text
// cannot tell egui's viewport command from a child-process command. So the one place that
// closes this window names the type by an alias that says what it is — and this comment
// does not spell the process type out, because the spec reads lines and not syntax trees.
use eframe::egui::ViewportCommand as Viewport;

use crate::data::{Archive, Article, Counts, Monitor, Topic};
use crate::theme;
use crate::views::graph::{self, GraphView};
use crate::views::overview::{Door, Kind, Overview, Snapshot};
use crate::views::{articles::ArticlesView, nf};

/// The pages, in the order the rail lists them.
///
/// Public with [`PAGES`], `label` and [`Atlas::set_page`]: a test of the *window* has to
/// be able to ask for a page the way the rail does, and driving the rail through a
/// synthetic pointer would only be a longer spelling of this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Overview,
    Articles,
    /// The source page itself, in the window's own engine (`crate::webview`), for a story
    /// whose address the reader pressed. A page and not a pane: it is reached from a link and
    /// left with "Back to …", and on a build without an engine it is the archive's copy of
    /// the page's words, said to be exactly that.
    Reader,
    Graph,
    Topics,
    Monitors,
    Chat,
    Settings,
}

impl Page {
    /// The rail's label: sentence case, one word.
    pub fn label(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Articles => "Articles",
            Page::Reader => "Reader",
            Page::Graph => "Graph",
            Page::Topics => "Topics",
            Page::Monitors => "Monitors",
            Page::Chat => "Chat",
            Page::Settings => "Settings",
        }
    }

    /// The page's mark in a folded rail: the first letter of [`Self::label`], with the whole
    /// name on hover. There is no icon set in this tree, and inventing one would be a
    /// redesign; an initial is the name the rail already shows, shortened until it fits a
    /// 64 px column.
    pub fn mark(self) -> &'static str {
        match self {
            Page::Overview => "O",
            Page::Articles => "A",
            Page::Reader => "R",
            Page::Graph => "G",
            Page::Topics => "T",
            Page::Monitors => "M",
            Page::Chat => "C",
            Page::Settings => "S",
        }
    }
}

/// The page a story was opened from, with the three narrowing choices that page had.
///
/// A visit to a story is not a place in the app: it is a look at one row of the archive, and
/// leaving it has to put the reader back where they were. Storing the *choices* (a rollup
/// chip, a topic, a monitor, a search) as well as the page is what makes the return exact —
/// a country chip that was in force when the reader left is in force when they come back,
/// because nothing else remembers it once the Articles page has cleared it.
#[derive(Clone, Debug, PartialEq)]
struct Origin {
    page: Page,
    place: Option<Door>,
    topic: Option<i64>,
    monitor: Option<i64>,
    needle: String,
}

/// Every page, in the order the rail draws them. Public so a test can walk the rail
/// headlessly instead of listing six names and going stale when a seventh arrives.
pub const PAGES: [Page; 8] = [
    Page::Overview,
    Page::Articles,
    Page::Reader,
    Page::Graph,
    Page::Topics,
    Page::Monitors,
    Page::Chat,
    Page::Settings,
];

/// Where the Chat page is told the model answers: `ATLAS_MODEL_URL` when the reader set
/// one, else the port in `data/config.json` (`llama_port`), else this documented default.
///
/// The configured port is what makes Chat work out of the box: `data/config.json` says
/// `8081` in this checkout (and `atlas start` starts the server there), while this
/// constant says `8080`. Hard-coding one of the two meant the page pointed at an address
/// nothing was listening on, and the reader got "not answering" from a server that was
/// running the whole time.
///
/// Nothing here dials: the address is handed to the Chat view, which passes it through
/// `crate::http::endpoint` — the one place a URL is judged — and reports a refusal in its
/// own sentence.
const DEFAULT_MODEL_URL: &str = "http://127.0.0.1:8080";

/// What a "Ask about this" press in the Reader asks the model.
///
/// One sentence saying what the reader wants done with the page, then the three facts the
/// archive has about it. Written as a question and not as a summary instruction, because the
/// chat is a conversation: the reader can go on from there. Empty fields are left out rather
/// than shown as gaps — a story with no title is a story whose address is the only name it
/// has, and the address is always there.
fn question_about(story: &crate::views::reader::Story) -> String {
    let mut question = String::from(
        "Read this page and tell me what it says, what it changes, and how it connects to the \
         stories in the archive.",
    );
    let title = story.title.trim();
    let source = story.source.trim();
    let published = story.published.trim();
    if !title.is_empty() || !source.is_empty() || !published.is_empty() {
        question.push_str("\n\n");
        let mut facts: Vec<&str> = Vec::new();
        if !title.is_empty() {
            facts.push(title);
        }
        if !source.is_empty() {
            facts.push(source);
        }
        if !published.is_empty() {
            facts.push(published);
        }
        question.push_str(&facts.join(" · "));
    }
    if !story.url.trim().is_empty() {
        question.push('\n');
        question.push_str(story.url.trim());
    }
    question
}

/// The model address the window was told to use.
///
/// Pure, so a test can ask it without a process-wide variable or the checkout's config
/// file: `set` is `ATLAS_MODEL_URL`, `port` is `crate::assets::model_port()`.
fn model_url_from(set: Option<&str>, port: Option<u16>) -> String {
    if let Some(url) = set {
        if !url.trim().is_empty() {
            return url.trim().to_string();
        }
    }
    match port {
        Some(port) => format!("http://127.0.0.1:{port}"),
        None => DEFAULT_MODEL_URL.to_string(),
    }
}

fn model_url() -> String {
    model_url_from(
        std::env::var("ATLAS_MODEL_URL").ok().as_deref(),
        crate::assets::model_port(),
    )
}

/// What the status row says while the Chat page is open.
///
/// Fixed text, and not the address: `ATLAS_MODEL_URL` is the reader's own configuration,
/// and a configuration value can be a credential (`http://alice:secret@127.0.0.1:8080`)
/// or a control character that turns a one-line label into two. The address is dialled by
/// the Chat view, where `crate::http::endpoint` refuses a bad one with a sentence of its
/// own that never repeats the value.
const CHAT_NEEDS_MODEL: &str = "chat needs the local model";

/// What one read of the archive was *for*: the window, the search box, the rollup door, and
/// the rows the reader has open.
///
/// **What is deliberately not in it: the page that is up.** Every read fills the same rows
/// (`pins`, `countries`, `articles`, `counts`, `topics`, `monitors`, the graph), so the page
/// decides what is *drawn* and never what is *queried*. It used to be a field here, and that
/// cost more than one query: the reader's camera on the Graph page (zoom, pan) is state in
/// `views::graph`, every read ends in `GraphView::fill`, and `fill` frames the plate from
/// scratch — so switching Overview → Graph threw the gesture away. Measured by
/// `tests/layout_screens.rs`: after a wheel and a drag the plate sat 92.16 px from a fresh
/// window's picture; after Overview → Graph it was 0.00 px from the fresh picture, i.e. the
/// camera was gone. With the page out of this value a page switch that changes nothing else
/// is a frame with no query, and the camera it did not touch is still the reader's.
///
/// A read happens when this value changes and at no other time. Keeping it as a value — rather
/// than a "needs reload" flag each site has to remember to set — is what makes the frame
/// read-free: a site that changes the window or the page needs no second line, because the
/// next frame compares what is loaded with what is wanted ([`needs_read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Demand {
    hours: i64,
    /// The search box, normalised by [`needle_key`]: case and surrounding space are
    /// the same search, not a new read.
    needle: String,
    /// The **rollup row** the reader opened — a chip under the Overview's map — as the door
    /// the page named it: a country (`iso`, counted by `Archive::countries`) or a place
    /// (a label, the grouping the map's pins use).
    ///
    /// It carries *what the word is*, not just the word, because the two groupings are
    /// different sets of rows and `USA` is a valid word of both kinds. The kind is part of the
    /// demand for the same reason: switching from the country `USA` to the place `USA` is a
    /// different list, so it has to be a different read.
    ///
    /// Not normalised by [`needle_key`]: the search box is matched case-insensitively by the
    /// archive, but this is an **equality** against a stored word (`a.iso = ?` /
    /// `coalesce(nullif(place,''), nullif(iso,''), '') = ?`), so lowercasing it — as this did
    /// — made every mixed-case place label match nothing.
    place: Option<Door>,
    /// The open topic, if the reader has one open. Carried **whether or not Topics is the
    /// page that is up**, and the same for [`Self::monitor`]: the stories under an open row
    /// are fetched once and stored in their own field, so which page is showing can never
    /// turn the same open row into a second read. The lists are drawn by the page that owns
    /// them; the read does not need to know which one that is.
    topic: Option<i64>,
    /// The open monitor, on the same terms.
    monitor: Option<i64>,
}

impl Demand {
    fn of(
        hours: i64,
        needle: &str,
        place: Option<Door>,
        topic: Option<i64>,
        monitor: Option<i64>,
    ) -> Demand {
        Demand {
            hours,
            needle: needle_key(needle),
            place: place.filter(|door| !door.word.trim().is_empty()),
            topic: topic.filter(|id| *id > 0),
            monitor: monitor.filter(|id| *id > 0),
        }
    }
}

/// The search box as a comparison key.
///
/// `atlas_data::Archive::articles` matches case-insensitively, so "Chip" and "chip" are
/// one query; typing a trailing space is not a reason to hit the database again.
fn needle_key(needle: &str) -> String {
    needle.trim().to_lowercase()
}

/// What a read's briefing is about: the scope word `briefings` is keyed by, the key inside it,
/// and the words the card shows for it.
///
/// The narrowing choices are the scope, in the order the page applies them — a chip first (the
/// reader picked a place and then read the news from it), then an open topic, then an open
/// monitor, and the whole window when nothing is narrowed. The three strings are exactly what
/// [`crate::views::brief::Ask`] takes, so the card cannot be asked about one scope while the
/// list below it shows another.
///
/// A note is the reader's own word for the scope where there is one. A country chip carries the
/// `iso` the archive groups by (that is also the key the briefing is stored under), and a place
/// carries its label; both are the words the chip printed, so the card and the chip agree.
fn brief_of(demand: &Demand) -> (&'static str, String, String) {
    if let Some(door) = demand.place.as_ref() {
        let scope = match door.kind {
            Kind::Country => "country",
            Kind::Place => "place",
        };
        return (scope, door.word.clone(), door.word.clone());
    }
    if let Some(id) = demand.topic {
        return ("topic", id.to_string(), "this topic".to_string());
    }
    if let Some(id) = demand.monitor {
        return ("monitor", id.to_string(), "this monitor".to_string());
    }
    ("window", String::new(), "the window".to_string())
}

/// Is a read needed? `dirty` is Refresh — the one request no value can express, because
/// the archive may have been written to by another process since the last read.
fn needs_read(loaded: Option<&Demand>, wanted: &Demand, dirty: bool) -> bool {
    dirty || loaded != Some(wanted)
}

/// The one place a rollup door becomes a read: **the grouping that counted the chip**.
///
/// A country chip's number comes from `Archive::countries`, which groups by `iso`, so the
/// list has to be `Archive::country_stories` — the same rows, counted the same way. A place
/// chip's number is a place label, and the list is `Archive::place_stories`.
///
/// The defect this closes, measured by `tests/map_audit.rs` on the shipped archive (72 h
/// window): the `USA` chip said **105** stories, `country_stories("USA")` held 105, and the
/// read the click made — `place_stories("USA")` — held **0**, because every United States
/// story names its place ("Washington", "New York") and so no row's *label* is `USA`. The
/// user's sentence for it was "I pick a filter and simply nothing comes". A single match on
/// the kind is what makes a chip unable to claim a story it cannot open: there is no second
/// place where the word and the read could disagree.
pub fn stories_for(archive: &Archive, door: &Door, hours: i64, limit: i64) -> Vec<Article> {
    match door.kind {
        Kind::Country => archive.country_stories(&door.word, hours, limit),
        Kind::Place => archive.place_stories(&door.word, hours, limit),
    }
}

/// How many rows a list read asks for: the cap the Articles page has always used.
///
/// It is also the bound on the promise "a count opens exactly its list": a country whose
/// window holds more stories than this opens the newest `LIST_ROWS` of them, and the strip
/// above the list then says `400 of 1 050 from USA` rather than printing the chip's number
/// over a shorter list. The list is what a reader can scroll; the number is always the
/// number of rows in it.
///
/// `pub` only so `tests/map_audit.rs` can measure the product's own cap instead of keeping a
/// copy of the number that could drift from it: the audit asserts each chip opens exactly
/// `count.min(LIST_ROWS)` rows.
pub const LIST_ROWS: i64 = 400;

/// How many country rows the Overview asks for: every country the window has a story in.
///
/// The rollup used to ask for 14, which was enough for the chips when a country's count came
/// from `articles.iso` alone (18 countries in the shipped window) and is not enough now that
/// the count is every place a story names: measured, **56 countries**, so 42 of them had no
/// chip and no door — a story that named one of them was counted nowhere on the page. The
/// number is a ceiling, not a page size: `Archive::countries` orders by count and the band
/// scrolls, so a smaller answer is simply drawn whole (0.62 ms for all 56 rows).
pub const COUNTRY_ROWS: i64 = 400;

/// The measured cost of a read, in whole milliseconds: a read faster than the clock's
/// display resolution says `<1 ms` rather than `0 ms`.
fn millis(spent: Duration) -> String {
    match spent.as_millis() {
        0 => "<1".to_string(),
        ms => ms.to_string(),
    }
}

pub struct Atlas {
    archive: Option<Archive>,
    /// The file the archive was opened from, kept even when the open failed: the status
    /// row names it, and the Chat page reads the same file the pages are drawn from —
    /// [`Atlas::reload`] hands it over, so there is one answer to "which archive is this
    /// window about" rather than one per page.
    archive_path: PathBuf,
    problem: String,
    /// What the reader asked for; `loaded_for` says what is actually loaded.
    page: Page,
    hours: i64,
    /// Everything the pages draw, read once per [`Demand`] and handed to the views as a
    /// reference. The frame clones none of it.
    snapshot: Snapshot,
    topics: Vec<Topic>,
    monitors: Vec<Monitor>,
    /// The address the Chat page is told the model answers on: dialled by that view, and
    /// never printed here — the value comes from `ATLAS_MODEL_URL`, so it can carry a user
    /// name, a password or a newline ([`CHAT_NEEDS_MODEL`]).
    model_url: String,
    /// The search template the archive's `meta` row names, read with the rest of the
    /// snapshot: `None` means nobody chose one and the built-in engine answers. Read here
    /// and not in the page, because a frame reads nothing (the Settings page would
    /// otherwise open a second connection to say which engine is in use).
    search_engine: Option<String>,
    /// The tokens one chat request may spend, as the reader set them.
    ///
    /// Read from the archive's `meta` table with the rest of the snapshot — the agent
    /// mirrors `model_reasoning` and `model_answer` there when it applies a settings
    /// request — and built **once per reload** rather than per frame: the Chat page would
    /// otherwise read two rows on every repaint to send a number that changes only when
    /// Save is pressed. A machine nobody has configured gets
    /// [`crate::http::Budget::SHIPPED`], which is exactly what this window sent before
    /// Settings could change it.
    budget: crate::http::Budget,
    overview: Overview,
    articles_view: ArticlesView,
    /// The **rollup row** the reader is reading the news from, if any — set by a chip under
    /// the Overview's map and cleared by the strip's own button on the Articles page. It is a
    /// [`Door`] and not a word, because a country and a place are two reads
    /// ([`stories_for`]).
    place: Option<Door>,
    /// The topic whose row is open, and its stories: the page keeps the open row in the
    /// context, the shell reads it and asks the archive for that topic's stories. Two
    /// fields, not one, so a stale read is drawn as "nothing read" rather than as another
    /// topic's news.
    topic_open: Option<i64>,
    topic_stories: Vec<crate::data::Article>,
    /// The same pair for Monitors.
    monitor_open: Option<i64>,
    monitor_stories: Vec<crate::data::Article>,
    graph: GraphView,
    /// Where the reader came from, when this page is a story opened from another one.
    ///
    /// A reader who clicks a story in the Graph must be able to come back to the Graph —
    /// with the node they had selected and the camera they had set — and a reader who clicks
    /// a story under a country chip must find that chip still in force when they come back.
    /// One value, pushed when the page changes and consumed by [`Atlas::go_back`], instead of
    /// a `page` field that a click overwrites and nothing remembers.
    origin: Option<Origin>,
    /// The story the frame that opens the Articles page must bring into view, consumed by
    /// the page on that same frame (`views::articles::reveal`).
    reveal: Option<i64>,
    /// The page the Reader is showing, as the archive had it when the reader pressed the
    /// story's address: the toolbar's three facts, and the words a build without an engine
    /// falls back to. Kept here rather than re-read every frame, for the reason every other
    /// row is: a frame reads nothing.
    reader_story: Option<crate::views::reader::Story>,
    /// The engine's page, when this build has one (`crate::webview::open`). `None` means the
    /// build has no engine, or the engine refused to make one — and then the Reader draws the
    /// archive's copy and says so, instead of pretending the screen is the page.
    reader_page: Option<crate::webview::Page>,
    /// Whether the archive holds a newer copy of the open page than the one on screen. Read
    /// when the archive is read ([`Atlas::reload`]), never per frame.
    reader_stale: bool,
    /// The **briefing** for the scope the Articles page is about: the model's prepared sentence
    /// over the stories the page is showing, written by the agent (`atlas-ingest`), refreshed
    /// by it (`atlas-monitor`), read here once per [`Demand`].
    ///
    /// Read in the shell and not in the page, for the reason every other row is: a frame reads
    /// nothing. `brief_scope`/`brief_key`/`brief_note` are what the read was *about*, kept so
    /// the card can name the scope and ask for a refresh without re-deriving it from three
    /// fields in a draw call.
    briefing: Option<crate::data::Briefing>,
    /// The whole-window briefing: what the Overview draws, read with the rest of the snapshot
    /// so switching pages never asks the archive twice for the same answer.
    window_briefing: Option<crate::data::Briefing>,
    /// The newest article id at the last read ([`crate::views::brief::stale`] compares it with
    /// the briefing's watermark).
    newest_story: i64,
    brief_scope: &'static str,
    brief_key: String,
    brief_note: String,
    /// The `(scope, key, hours)` slots this window has already asked the agent for, so a page
    /// with no briefing asks once and not once per read.
    ///
    /// A briefing is what the app is *for*: the reader should not have to press a button to see
    /// the model's sentence about the country they just clicked. So a scope that has stories and
    /// no row asks for one on the read that discovers it — the agent writes it, and the next
    /// read draws it. The set is the guard: without it every read of every page would queue the
    /// same request, and a queue of duplicates is a queue the agent spends its turns on.
    brief_asked: std::collections::HashSet<(String, String, i64)>,
    /// Whether this window asks the agent for a briefing it does not have yet.
    ///
    /// **On for the shipped window, off for a fixture.** The ask writes a row into the request
    /// queue, which is a real file with real consequences — a test that built a window over a
    /// temporary archive must not leave a request in the checkout's queue, and a test that
    /// asserts "this page has no briefing" must not turn its own read into one. `Atlas::new`
    /// turns it on; `Atlas::with_archive` leaves it off, so every test is quiet unless it asks.
    asks_for_briefings: bool,
    chat: crate::views::chat::ChatState,
    /// The demand the data above answers, and what reading it cost.
    loaded_for: Option<Demand>,
    /// Set by Refresh: the archive may have changed under us, whatever the demand says.
    dirty: bool,
    /// How many times the archive was read since the window opened. The status row says
    /// it, so a per-frame read would be impossible to hide.
    reads: u32,
    read_spent: Duration,
    /// The one file this window may write: the request queue (`src/requests.rs`).
    ///
    /// Named once, at construction, and handed to the pages that offer an action — so the
    /// window has exactly one write target and every page writes to the same one. It is a
    /// field rather than a constant read in the views so that a test can point a whole
    /// window at a temporary queue instead of the checkout's `data/`.
    queue: PathBuf,
    /// The rail's folded state once the reader has asked for one; `None` means "the window
    /// decides" ([`Atlas::RAIL_FOLDS_BELOW`]). Set by [`Atlas::toggle_rail`].
    rail_choice: Option<bool>,
    /// The first screen, until the reader accepts the terms ([`crate::views::license`]).
    ///
    /// `Some` means the gate is up: the rail, the pages and the status row are not drawn at
    /// all, so a window nobody has accepted anything in cannot be mistaken for a window in
    /// use. `None` means the acceptance is on disk for the current document version and the
    /// workspace draws as it always did.
    gate: Option<crate::views::license::Gate>,
    /// Read-only watcher for `data/agent-status.json` (live AI phase, tool log, reasoning).
    activity: crate::activity::Watch,
    /// Whether the expandable AI Activity & Reasoning drawer is open.
    activity_open: bool,
    /// True after the user manually hides the drawer, so auto-open respects their choice.
    activity_dismissed: bool,
    /// Last seen `(mtime, len)` stamp of `atlas.db` and `atlas.db-wal` for live auto-refresh.
    archive_stamp: Option<(Option<std::time::SystemTime>, u64, Option<std::time::SystemTime>, u64)>,
    /// Last time `archive_stamp` was checked.
    last_stamp_check: Instant,
}

impl Atlas {
    /// The window the shipped app opens: the theme for this context, and the archive at
    /// [`crate::assets::archive`].
    ///
    /// Two lines, because everything else is [`Atlas::with_archive`] — which is the
    /// difference between a window that can only be tested by looking at it and one that
    /// can be driven from a test against an archive of the test's own.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Atlas {
        theme::install(&cc.egui_ctx);
        // The shipped window is the one that asks for the briefings it does not have: it is the
        // window a reader uses, and the request queue it writes is the checkout's own.
        Atlas::with_archive(crate::assets::archive())
            .with_terms()
            .asking_for_briefings()
    }

    /// Show the terms unless this machine has already accepted this version of them.
    ///
    /// Called by [`Atlas::new`], which is the window a person opens. The record is read once,
    /// before the first frame, from the same small database the window already writes
    /// (`crate::requests`): a reader who accepted yesterday sees the workspace immediately, a
    /// reader who has not sees the document, and an unreadable record shows the document
    /// again — the safe direction, because "cannot tell" is not "consented".
    pub fn with_terms(mut self) -> Atlas {
        let accepted = crate::requests::accepted_at(&self.queue, crate::views::license::VERSION);
        self.gate = (!accepted).then(crate::views::license::Gate::default);
        self
    }

    /// The same window, opening `path` instead of the checkout's archive.
    ///
    /// Reads nothing outside `path`: [`crate::data::Archive::open`] only opens, and
    /// `sync` at the end is the same read `new` has always done, so the first frame has
    /// its data and the status row has its numbers.
    pub fn with_archive(path: PathBuf) -> Atlas {
        let (archive, problem) = match Archive::open(&path) {
            Ok(archive) => (Some(archive), String::new()),
            Err(problem) => (None, problem),
        };
        let archive_path_for_activity = path.clone();
        let mut atlas = Atlas {
            archive,
            // Cloned once, for the graph below: the relation panel reads the archive the
            // *window* opened, not `crate::assets::archive()` — the machine's live file —
            // so a window over a fixture can reach the panel at all.
            archive_path: path.clone(),
            problem,
            page: Page::Overview,
            hours: 72,
            snapshot: Snapshot {
                pins: Vec::new(),
                countries: Vec::new(),
                articles: Vec::new(),
                counts: Counts::default(),
                hours: 72,
            },
            topics: Vec::new(),
            monitors: Vec::new(),
            model_url: model_url(),
            search_engine: None,
            budget: crate::http::Budget::SHIPPED,
            overview: Overview::new(),
            articles_view: ArticlesView::default(),
            place: None,
            topic_open: None,
            topic_stories: Vec::new(),
            monitor_open: None,
            monitor_stories: Vec::new(),
            graph: GraphView::default().with_archive(path),
            origin: None,
            reveal: None,
            reader_story: None,
            reader_page: None,
            reader_stale: false,
            briefing: None,
            window_briefing: None,
            newest_story: 0,
            brief_scope: "window",
            brief_key: String::new(),
            brief_note: "the window".to_string(),
            brief_asked: std::collections::HashSet::new(),
            asks_for_briefings: false,
            chat: crate::views::chat::ChatState::default(),
            loaded_for: None,
            dirty: false,
            reads: 0,
            read_spent: Duration::ZERO,
            rail_choice: None,
            queue: crate::requests::queue_path(),
            gate: None,
            activity: crate::activity::Watch::new(&archive_path_for_activity),
            activity_open: false,
            activity_dismissed: false,
            archive_stamp: crate::activity::archive_stamp(&archive_path_for_activity),
            last_stamp_check: Instant::now(),
        };
        // A fixture window has no gate: every test that drives this constructor is about the
        // workspace, and a screen that had to be clicked away first would be a test that
        // tests the terms instead. The shipped window arms it in [`Atlas::new`].
        atlas.sync();
        atlas
    }

    /// The same window, queueing its requests into `queue` rather than the checkout's
    /// `data/requests.db`.
    ///
    /// Exists for the same reason [`Atlas::with_archive`] does: a headless frame that presses
    /// a request link must not write the live checkout. The shipped constructors
    /// ([`Atlas::new`], [`Atlas::with_archive`]) always name the one real queue.
    pub fn with_queue(mut self, queue: PathBuf) -> Atlas {
        self.queue = queue;
        self
    }

    /// Ask the agent for a briefing this window does not have yet — the switch the shipped
    /// window turns on and a fixture leaves off.
    ///
    /// Public because the *behaviour* is worth measuring without a screen: a test that wants to
    /// see the request has to be able to turn the ask on, and a test that measures anything else
    /// must not find an unexpected row in its queue. [`Atlas::new`] calls this; nothing else in
    /// the window does.
    pub fn asking_for_briefings(mut self) -> Atlas {
        self.asks_for_briefings = true;
        self
    }

    /// Show `page`, as the rail does.
    ///
    /// The one thing a page switch decides beyond the field: a graph that was asleep
    /// gets a little heat back, so the layout settles after the click rather than
    /// appearing frozen. The rail calls this, so a test that calls it takes the same
    /// path a reader takes.
    pub fn set_page(&mut self, page: Page) {
        self.page = page;
        if page == Page::Graph {
            self.graph.alpha = self.graph.alpha.max(0.35);
        }
        self.close_reader_unless_here();
    }

    /// Close the source page's engine view once the Reader is no longer the page.
    ///
    /// The engine paints **over** the window, not inside a layout: an egui panel cannot clip a
    /// child web view, so a view left alive when the reader goes to the map keeps drawing its
    /// page on top of the map, and no click reaches the plate underneath. Measured in the
    /// shipped window: the reader pressed "Map" with a source page open and the page stayed,
    /// on top, with nothing in the window able to close it.
    ///
    /// The view is *dropped*, not hidden: leaving the page is leaving the page, and a hidden
    /// view would still hold a live document (cookies, a running script) for a page the reader
    /// has walked away from. The story goes with it, so pressing "Reader" in the rail afterwards
    /// shows the page's own invitation instead of a stale address with no engine behind it.
    fn close_reader_unless_here(&mut self) {
        if self.page != Page::Reader {
            self.reader_page = None;
            self.reader_story = None;
        }
    }

    /// The page being shown.
    pub fn page(&self) -> Page {
        self.page
    }

    /// Read the archive only if the demand changed since the data in hand was read.
    ///
    /// Called at the top of every frame and again after the rail. A frame that changed
    /// nothing builds one short comparison key (the trimmed, lower-cased search box) and
    /// touches no database; the search box's case is normalised because the query matches
    /// it that way, so typing a capital is not a second read. **A page switch is not part of
    /// the key** ([`Demand`]): it changes what is drawn, not what is queried, so flipping to
    /// Graph costs no query and cannot disturb the camera the reader left there.
    fn sync(&mut self) {
        let wanted = Demand::of(
            self.hours,
            &self.articles_view.needle,
            self.place.clone(),
            self.topic_open,
            self.monitor_open,
        );
        if !needs_read(self.loaded_for.as_ref(), &wanted, self.dirty) {
            return;
        }
        self.reload(wanted);
    }

    /// One read of the archive, measured.
    ///
    /// The entities and edges are moved into the graph rather than cloned into it, and the
    /// rows a page shows are stored here — so what a frame draws is already in memory, and
    /// a 120-node graph animates without re-reading. The only copy this function makes is
    /// the struct field itself: `Snapshot` takes the rows by value.
    fn reload(&mut self, wanted: Demand) {
        let started = Instant::now();
        self.dirty = false;
        self.reads = self.reads.saturating_add(1);
        // Chat asks the model about the same file, over the same window, as the pages are
        // drawn from — and this is the one place a window is settled, so a rail that
        // changed the hours reaches Chat through the demand, with no rule of its own
        // (`ChatState::set_window` ignores a window it already holds).
        self.chat.set_window(&self.archive_path, wanted.hours);
        let Some(archive) = self.archive.as_ref() else {
            // No archive: there is nothing to read and `problem` already says why. The
            // demand is recorded anyway, or the frame would try again every 16 ms.
            self.loaded_for = Some(wanted);
            self.read_spent = started.elapsed();
            return;
        };
        let pins = archive.pins(wanted.hours, 400);
        // A rollup row narrows the list through **the grouping that counted it**: a country
        // chip is `Archive::countries`' count, so it opens `Archive::country_stories`
        // (`iso`); a place label opens `Archive::place_stories` (the label expression the
        // pins are built from). One function, [`stories_for`], so the door and the count
        // cannot drift apart: on the shipped archive the `USA` chip counted 105 stories and
        // the *place* read returned 0, because every United States story names a city.
        let mut articles = match wanted.place.as_ref() {
            Some(door) => stories_for(archive, door, wanted.hours, LIST_ROWS),
            None => archive.articles(wanted.hours, LIST_ROWS, &wanted.needle),
        };
        // Every dot on the map is a door to one story, and that story is the newest at
        // its place — which is *not* necessarily one of the 400 newest rows the list is
        // capped at. Without this, a click on a busy window landed on "That story is no
        // longer in the window.": the page was told to show an id its list had never
        // read. The list is topped up by key, once per read, and stays newest-first.
        //
        // **Only for an unfiltered read.** A list narrowed by a rollup chip is *the chip's
        // list* — the strip above it prints the number of rows in it, so a story appended
        // from another country would make that number a lie ("106 from USA" over a list of
        // 105 US stories plus one from Madrid). The dots' destinations matter on the
        // unfiltered list, which is the one a dot click opens: a dot click sets no door.
        //
        // The story the reader has open is topped up either way: the pane is showing it, and
        // "That story is no longer in the window" about a story on screen is never true.
        let held: std::collections::HashSet<i64> = articles.iter().map(|row| row.id).collect();
        let open = self.articles_view.selected.unwrap_or(0);
        let mut missing: Vec<i64> = Vec::new();
        if wanted.place.is_none() {
            missing.extend(pins.iter().map(|pin| pin.article));
        }
        missing.push(open);
        let missing: Vec<i64> = missing
            .into_iter()
            .filter(|id| *id > 0 && !held.contains(id))
            .collect();
        if !missing.is_empty() {
            articles.extend(archive.articles_by_ids(&missing));
            articles.sort_by(|left, right| {
                (right.published.as_str(), right.id).cmp(&(left.published.as_str(), left.id))
            });
        }
        self.snapshot = Snapshot {
            pins,
            countries: archive.countries(wanted.hours, COUNTRY_ROWS),
            articles,
            counts: archive.counts(),
            hours: wanted.hours,
        };
        self.search_engine = archive.meta("search_engine");
        // The briefing for what this read is about, and the whole window's: one read each, on
        // the frame that already reads, so the cards cost no query of their own. `hours` is
        // part of the briefing's key, so a page that asks for 24 h does not draw the 72-hour
        // sentence over a 24-hour list.
        let brief = brief_of(&wanted);
        self.brief_scope = brief.0;
        self.brief_key = brief.1;
        self.brief_note = brief.2;
        self.briefing = archive.briefing(self.brief_scope, &self.brief_key, wanted.hours);
        self.window_briefing = if self.brief_scope == "window" {
            self.briefing.clone()
        } else {
            archive.briefing("window", "", wanted.hours)
        };
        self.newest_story = archive.newest_id();
        // Whether the copy of the page the Reader is showing is still the archive's newest.
        // Compared, not re-read: the row is already here, and a story the window no longer
        // holds has nothing to be stale against.
        self.reader_stale = self
            .reader_story
            .as_ref()
            .filter(|story| story.id > 0)
            .and_then(|story| {
                let row = self
                    .snapshot
                    .articles
                    .iter()
                    .find(|article| article.id == story.id)?;
                let newest = crate::views::reader::Story::of(row);
                Some(newest.text != story.text)
            })
            .unwrap_or(false);
        // The two numbers a chat request carries, mirrored here by the agent so this
        // read-only window can send what the reader chose (it never opens config.json).
        self.budget = crate::http::Budget::from_meta(
            archive.meta("model_reasoning").as_deref(),
            archive.meta("model_answer").as_deref(),
        );
        self.topics = archive.topics();
        self.monitors = archive.monitors();
        // The open rows' stories, read once per demand. The page does not enter this: the
        // list belongs to the row, and the row belongs to the demand ([`Demand::topic`] /
        // [`Demand::monitor`]) whether or not its page is the one on screen. A topic with no
        // stories is an empty list and the page says so; a monitor reads the pages it raised
        // as alerts as well as the ones it merely judged (`monitor_stories` is their union).
        self.topic_stories = match wanted.topic {
            Some(id) => archive.topic_articles(id, wanted.hours, 40),
            None => Vec::new(),
        };
        self.monitor_stories = match wanted.monitor {
            Some(id) => archive.monitor_stories(id, wanted.hours, 40),
            None => Vec::new(),
        };
        let entities = archive.entities(wanted.hours, 120);
        // The plate is the reader's: `GraphView::fill` re-frames it (zoom 1, pan 0), so it is
        // called only when the picture would actually change. The same entities at the same
        // strengths are the same plate, and re-filling them would throw away a zoom the
        // reader made — the camera loss a *page switch* used to cause, one read later. Which
        // page is up never reaches this line at all: the page is not part of a demand.
        let unchanged = entities.len() == self.graph.entities.len()
            && entities
                .iter()
                .zip(&self.graph.entities)
                .all(|(new, held)| new.id == held.id && new.count == held.count);
        if !unchanged {
            let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
            let edges = archive.edges(&ids, 900);
            self.graph.fill(entities, edges);
        }
        // Recorded last, and moved in: the demand is one value, not a copy of one.
        self.loaded_for = Some(wanted);
        self.read_spent = started.elapsed();
    }

    /// The page's inner margin, both sides and both ends: the page box is the central panel's
    /// fill shrunk by this. Named because the layout tests measure the page box, and a margin
    /// that exists in the frame but not in the measurement would make them disagree.
    const PAGE_MARGIN_X: f32 = 20.0;
    const PAGE_MARGIN_Y: f32 = 16.0;

    /// The rail with its words: measured at 188 px, which was 26.9 % of a 700 px window.
    const RAIL_WIDTH: f32 = 188.0;
    /// The rail folded to marks: the logo, one initial per page, the three windows and
    /// Refresh. 64 px, sixteen units of the spacing scale.
    const RAIL_FOLDED: f32 = theme::space::UNIT * 16.0;
    /// The plate's fair share of the page box once the relations column has taken its width.
    /// Three fifths: the canvas is the page, the column is a reading aid beside it.
    const PLATE_SHARE: f32 = 3.0 / 5.0;
    /// Below this window width the rail folds itself.
    ///
    /// Derived from the plate's fair share ([`Self::PLATE_SHARE`]) with the column beside it:
    /// the plate gets `page_box − PANEL_WIDTH − GRAPH_GAP`, so a window narrower than
    /// `(PANEL_WIDTH + GRAPH_GAP) / (1 − PLATE_SHARE)` plus the rail and the page's two 20 px
    /// margins — 1068 px — would leave the plate under three fifths while the rail took more
    /// of the window than the column does. Folding hands back 124 of the rail's 188 px, which
    /// does not reach three fifths on its own; below the Graph page's side-by-side floor
    /// (656 px of page box) the column stacks instead, so the two rules cover different
    /// ranges. The reader can override either with [`Self::toggle_rail`].
    const RAIL_FOLDS_BELOW: f32 =
        (graph::PANEL_WIDTH + GRAPH_GAP) / (1.0 - Self::PLATE_SHARE) + Self::RAIL_WIDTH + 40.0;

    fn rail(&mut self, ui: &mut egui::Ui) {
        // Folded, the rail is marks instead of words: the same controls in the same order
        // with the name of each on hover. There is no icon set in this tree and drawing one
        // would be a redesign, so a page's mark is the first letter of the label it already
        // has — `mark()` below.
        let folded = self.rail_folded(ui);
        let width = if folded { Self::RAIL_FOLDED } else { Self::RAIL_WIDTH };
        let margin = if folded { theme::space::SM } else { theme::space::MD };
        egui::Panel::left("rail")
            .exact_size(width)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PAGE)
                    .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE_SOFT))
                    .inner_margin(egui::Margin::symmetric(margin as i8, 14)),
            )
            .show(ui, |ui| {
                // The lockup: the mark, then the name, then what the window is. The mark
                // is drawn rather than lettered — see `logo` for why the name is not in
                // the drawing itself. Folded, the mark is the whole lockup.
                let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(28.0), egui::Sense::hover());
                crate::logo::paint(ui.painter(), mark, theme::INK);
                ui.add_space(theme::space::SM);
                if !folded {
                    // The name is a role, not a size typed here: `card_title` is 15 px of
                    // the text face in full ink, which is exactly what `.size(15.0)` +
                    // `.strong()` drew — egui 0.37 resolves `.strong()` to
                    // `widgets.active.text_color()` (`style.rs:1257`), the same INK body
                    // text already has, so the call was emphasis the pixels never showed.
                    ui.label(theme::card_title("ATLAS"));
                    ui.label(theme::dim("local news graph"));
                }
                ui.add_space(if folded { theme::space::MD } else { theme::space::LG });
                for page in PAGES {
                    let selected = self.page == page;
                    // A nav row is body text. Selected and unselected differ by ink
                    // (INK against DIM), never by "strong": egui draws `.strong()` as the
                    // ink body text already has, so it changed nothing while reading as
                    // emphasis. `body` is 13 px, the size the label had by default, so
                    // every row keeps its height.
                    let label = if folded { page.mark() } else { page.label() };
                    let text = if selected {
                        theme::body(label)
                    } else {
                        // The same role with its ink traded for the palette's secondary
                        // ink. `theme::dim` is not used here on purpose: that role drops a
                        // rung (12 px) and a shorter row moves every row below it.
                        theme::body(label).color(theme::DIM)
                    };
                    let row = ui.selectable_label(selected, text);
                    // The selection is carried by a 2 px bar as well as by the fill, because
                    // the fill's step from the page is 1.44:1 (`theme::SELECT` against
                    // `theme::PANEL`): a reader who cannot see that step still sees the bar.
                    // The shape is the graph's own selection mark (`views::graph`), and the
                    // width is the theme's focus width, so "this is the one you are on" looks
                    // the same everywhere in the window.
                    if selected {
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(row.rect.left() - theme::space::SM, row.rect.top() + 2.0),
                            egui::pos2(
                                row.rect.left() - theme::space::SM + theme::FOCUS_WIDTH,
                                row.rect.bottom() - 2.0,
                            ),
                        );
                        ui.painter().rect_filled(bar, egui::CornerRadius::ZERO, theme::INK);
                    }
                    // Folded, the initial is all that is drawn, so the name is on hover —
                    // and the row is still a focusable control, so the keyboard keeps it.
                    let row = if folded { row.on_hover_text(page.label()) } else { row };
                    if row.clicked() {
                        // No read here: the page is part of the demand, and `sync`
                        // compares it before anything is drawn.
                        self.set_page(page);
                        // The row's own selected ink was painted before this click was
                        // known, so the frame that marks it is the next one.
                        Self::rail_changed(ui);
                    }
                }
                ui.add_space(if folded { theme::space::MD } else { 14.0 });
                ui.separator();
                ui.add_space(theme::space::SM);
                let hours = [24_i64, 72, 168];
                if folded {
                    // One per line: three windows side by side do not fit 40 px, and a
                    // shorter label ("24h") is the same three windows, not a new scale.
                    for window in hours {
                        let active = self.hours == window;
                        let text = if active {
                            theme::body(Self::hours_mark(window))
                        } else {
                            theme::body(Self::hours_mark(window)).color(theme::DIM)
                        };
                        if ui.selectable_label(active, text).clicked() {
                            self.hours = window;
                            Self::rail_changed(ui);
                        }
                    }
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for window in hours {
                            let active = self.hours == window;
                            // Same rule as the page rows: a window is body text, and the one
                            // that is on is told apart by ink, not by a flag egui ignores.
                            let text = if active {
                                theme::body(Self::hours_label(window))
                            } else {
                                theme::body(Self::hours_label(window)).color(theme::DIM)
                            };
                            if ui.selectable_label(active, text).clicked() {
                                self.hours = window;
                                Self::rail_changed(ui);
                            }
                        }
                    });
                }
                ui.add_space(theme::space::SM);
                ui.separator();
                ui.add_space(theme::space::SM);
                // Folded, the word "Refresh" would be clipped in a 40 px column; the
                // caption role is the smallest on the ladder and fits, and the button keeps
                // its word — an initial here would be a new vocabulary.
                let refresh = if folded { theme::caption("Refresh") } else { theme::body("Refresh") };
                if ui.button(refresh).clicked() {
                    // The only request that is not a value: another process may have
                    // written to the archive since this window read it.
                    self.dirty = true;
                    Self::rail_changed(ui);
                }
                // Folded, the whole counts block is hidden, and it stays hidden: a 64 px
                // column has no room for "5 connections", and a count drawn as a bare
                // number with a destination on hover would be a riddle rather than a door.
                // (The folded rail still reaches all four pages through their marks.)
                if !folded {
                    ui.add_space(10.0);
                    // Each count is a claim about one page, so each is a door to that page:
                    // the story total opens Articles, entities and connections open Graph,
                    // topics open Topics, monitors open Monitors. A blind-click audit found
                    // all five inert — text that looked like a control and answered nothing.
                    // Hover says where the click goes; the ink and the sizes are unchanged,
                    // so the rail looks exactly as it did.
                    let counts = self.snapshot.counts;
                    let doors = [
                        (theme::number(&nf(counts.relevant)), Page::Articles),
                        (
                            theme::dim(&format!("{} entities", nf(counts.entities))),
                            Page::Graph,
                        ),
                        (
                            theme::dim(&format!("{} connections", nf(counts.links))),
                            Page::Graph,
                        ),
                        (
                            theme::dim(&format!("{} topics", nf(counts.topics))),
                            Page::Topics,
                        ),
                        (
                            theme::dim(&format!("{} monitors", nf(counts.monitors))),
                            Page::Monitors,
                        ),
                    ];
                    for (text, page) in doors {
                        if Self::rail_door(ui, text, page) {
                            self.set_page(page);
                        }
                    }
                }
            });
    }

    /// One count in the rail, drawn exactly as the plain label it replaced, but answering a
    /// click with the page it counts and naming that page on hover.
    ///
    /// `selectable_label(false, …)` and not `ui.link`: a link is underlined ink and reads as
    /// a reference, while these sit in a rail whose other doors are rows — same vocabulary,
    /// same hover fill, and the selected state stays `false` because a total is not a
    /// selection.
    fn rail_door(ui: &mut egui::Ui, text: egui::RichText, page: Page) -> bool {
        ui.selectable_label(false, text)
            .on_hover_text(Self::door_hover(page))
            .clicked()
    }

    /// What a count's hover says: the destination, in the words the rail uses for it.
    fn door_hover(page: Page) -> String {
        format!("Open the {} page", page.label())
    }

    /// The window's label in the rail's full width: "24 h", "3 d", "7 d".
    fn hours_label(hours: i64) -> &'static str {
        match hours {
            24 => "24 h",
            72 => "3 d",
            _ => "7 d",
        }
    }

    /// The same window, short enough for the folded rail: "24h", "3d", "7d".
    fn hours_mark(hours: i64) -> &'static str {
        match hours {
            24 => "24h",
            72 => "3d",
            _ => "7d",
        }
    }

    /// A rail control that changed the load asks for the frame that shows it.
    ///
    /// egui paints the frame it is handed, and a chip is laid out *before* the press on it is
    /// known: the ink that marks the window in force can only be right on the frame after the
    /// one that handled the click. egui does ask for that frame by itself when a widget is
    /// clicked (measured in this file's `a_chip_press_asks_for_the_next_frame_and_nothing_polls`),
    /// so this is not the only thing standing between the reader and a stale rail — it says the
    /// requirement where the state is changed, so the rail does not rest on an egui internal
    /// that a future version could drop. One request per press, never a timer: a quiet frame
    /// asks for nothing, so nothing here polls.
    fn rail_changed(ui: &egui::Ui) {
        ui.ctx().request_repaint();
    }

    /// The window for the status row: the rail's words for the three it offers, the hours
    /// themselves for any other value.
    ///
    /// [`Self::hours_label`] answers "7 d" for every unknown number, because the rail only
    /// ever asks it about the three chips and a fourth label would be a scale no reader
    /// asked for. A status row is a claim about a read that happened, so an unknown window
    /// has to be printed as what it is.
    fn window_words(hours: i64) -> String {
        match hours {
            24 | 72 | 168 => Self::hours_label(hours).to_string(),
            other => format!("{other} h"),
        }
    }

    /// Whether the rail shows marks instead of words: the reader's choice once they have
    /// made one, otherwise the window's width decides ([`Self::RAIL_FOLDS_BELOW`]).
    fn rail_folded(&self, ui: &egui::Ui) -> bool {
        self.rail_choice
            .unwrap_or_else(|| ui.available_width() < Self::RAIL_FOLDS_BELOW)
    }

    /// Ctrl/⌘ + B folds or unfolds the rail, and the choice then stands whatever the window
    /// does — an automatic rule with no way out is a rule that traps the reader.
    fn toggle_rail(&mut self, ui: &egui::Ui) {
        // The key event's *own* modifiers, not `input.modifiers`: egui updates the input's
        // modifier state from `Event::ModifiersChanged`, which a headless frame never sends,
        // so a check on `input.modifiers` would be untestable here and wrong on the first
        // frame after a focus change (`input_state/mod.rs:395-435`).
        let asked = ui.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::B,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command || modifiers.ctrl || modifiers.mac_cmd
                )
            })
        });
        if asked {
            self.rail_choice = Some(!self.rail_folded(ui));
        }
    }

    /// The bottom row: what is loaded, on the left; what this window cannot do without, on
    /// the right.
    ///
    /// **Both labels are placed by measured arithmetic, not by a nested layout.** The old
    /// version put the right label inside `ui.with_layout(right_to_left)` *inside* a
    /// `ui.horizontal`: the horizontal had already claimed the row, so the nested layout was
    /// handed a zero-width rect and `right_to_left` then placed the label at that rect's
    /// *right* edge — measured at 700×520, the label's 146 px galley started 13 px from the
    /// window edge and ran 133 px past the clip, on every page and at every size, and only
    /// the clip hid it. The rule that replaces it cannot fail that way: lay the right label
    /// out first (with the role's own font, so the measurement is the paint), give the left
    /// label exactly the room that is left, and put the right one at the row's own right
    /// edge. Neither rect can then leave the row, so neither label can leave the window; the
    /// left label — the one whose length varies with the archive's name — truncates instead.
    fn status_row(&self, ui: &mut egui::Ui) {
        let left = if self.problem.is_empty() {
            // The number is the **list's**, and the window is named beside it. It used to be
            // `counts.relevant` — every relevant story in the archive — which no chip can
            // change: the one number a reader can watch stood still while the list under it
            // changed, so a chip read as inert even when it had worked. The window is named
            // for the same reason: the three chips are three reads, and this row is where a
            // read says which one it was.
            let shown = self.snapshot.articles.len();
            let count = if shown as i64 >= LIST_ROWS {
                format!("{shown}+")
            } else {
                nf(shown as i64)
            };
            format!(
                "{} · {count} {} · {} window · read in {} ms · {} {}",
                self.archive
                    .as_ref()
                    .map(|archive| crate::assets::label(&archive.path))
                    .unwrap_or_default(),
                if shown == 1 { "story" } else { "stories" },
                Self::window_words(self.snapshot.hours),
                millis(self.read_spent),
                self.reads,
                if self.reads == 1 { "read" } else { "reads" }
            )
        } else {
            self.problem.clone()
        };
        // Only the Chat page needs the model, and it is the one page that cannot work
        // without it: the row says so instead of a chat box that would wait forever.
        let right: &str = if self.page == Page::Chat {
            CHAT_NEEDS_MODEL
        } else {
            "native window · local archive"
        };
        let row = ui.available_rect_before_wrap();
        let role = theme::role::SECONDARY;
        let right_width = ui
            .painter()
            .layout_no_wrap(right.to_owned(), role.font(), role.colour)
            .size()
            .x;
        // Never wider than the row: a long right label (another page may have one) is
        // truncated inside its own rect rather than pushed out of the window.
        let right_rect = egui::Rect::from_min_size(
            egui::pos2(row.right() - right_width.min(row.width()), row.top()),
            egui::vec2(right_width.min(row.width()), row.height()),
        );
        let left_rect = egui::Rect::from_min_max(
            row.left_top(),
            egui::pos2(
                (right_rect.left() - theme::space::MD).max(row.left()),
                row.bottom(),
            ),
        );
        Self::status_label(ui, left_rect, theme::dim(&left));
        Self::status_label(ui, right_rect, theme::dim(right));
        // The row is one line high whether or not a label had to truncate: without this the
        // panel would take its height from the left label's wrapped galley.
        ui.advance_cursor_after_rect(row);
    }

    /// One status label, in exactly `rect`, truncated inside it.
    ///
    /// `Ui::put`-shaped on purpose: the rect is an argument, not something the layout may
    /// renegotiate, which is the whole point of the fix above.
    fn status_label(ui: &mut egui::Ui, rect: egui::Rect, text: egui::RichText) {
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.add(egui::Label::new(text).truncate());
            },
        );
    }
}

impl eframe::App for Atlas {
    /// egui 0.37 hands the app the window's root `Ui` instead of the context, and every
    /// panel is shown inside it. `logic` (the pre-draw hook) is deliberately not used: the
    /// read stays in the frame, where 0.31 had it.
    ///
    /// The frame is handed on to [`crate::webview::attach`] once per frame: the engine builds
    /// a page from a *live* window handle, and this is the only place the app is given one.
    /// The call compiles on every platform — a build without an engine has nothing to
    /// remember and the function takes no argument it uses.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        crate::webview::attach(frame);
        self.draw(ui);
    }
}

impl Atlas {
    /// The whole frame: the rail, the status row, the page.
    ///
    /// `eframe::App::ui` is this plus one argument; splitting them is what lets a test
    /// draw the real window (`Atlas::with_archive`) through `egui::Context::run_ui`.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        // The first screen, and the only frame that is not the workspace: until the terms
        // are accepted nothing else is drawn, so there is no rail to click and no page to
        // read behind it.
        if self.gate.is_some() {
            self.draw_gate(ui);
            return;
        }
        // Poll live background AI activity (`data/agent-status.json`) and check whether
        // the archive on disk changed so newly added topics and articles appear automatically.
        self.poll_live_updates();
        // Before the rail draws: the window that opened last frame is already loaded, and
        // a row the reader opened last frame is already known here — so the read this frame
        // needs happens once, not one frame late.
        self.follow_open_rows(ui.ctx());
        // A "Read the source" press from any page: the view that drew the link could not open
        // a window from where it draws, so it left the address in the context and this is the
        // one place it is picked up — gated, and once, so a press cannot open two pages.
        if let Some(url) = crate::views::reader::take_source(ui.ctx()) {
            self.open_source(&url);
        }
        self.sync();
        // Before the rail is shown, because the shortcut changes its width for this frame.
        self.toggle_rail(ui);
        self.rail(ui);
        // And again after it, because the rail is where the window, the page and Refresh
        // change — the panel and the status row must not show the previous read. Both
        // calls are a comparison unless something was actually asked for.
        self.sync();

        // The briefing this page is about, if the agent has not written one: one request per
        // scope, on the frame the reader arrives, not on the read that filled the page.
        self.maybe_ask_for_briefing();

        // Esc leaves a story, on the two pages that have no open row of their own to close.
        // Topics, Monitors, Chat and the Graph already spend Esc on the row the keyboard is
        // on (`views/topics.rs`, `views/monitors.rs`, `views/chat.rs`, `views/graph.rs`), so
        // this runs for a story opened on its own or a source page read from one — the two
        // places where a reader who presses Esc means "back", and nothing happened before.
        if matches!(self.page, Page::Articles | Page::Reader)
            && self.origin.is_some()
            && ui.input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.go_back();
        }

        egui::Panel::bottom("status")
            .frame(
                egui::Frame::NONE
                    .fill(theme::PAGE)
                    .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE_SOFT))
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ui, |ui| self.status_row(ui));

        if self.activity.current().present || self.activity_open {
            egui::Panel::bottom("ai_activity")
                .frame(
                    egui::Frame::NONE
                        .fill(theme::PANEL)
                        .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE))
                        .inner_margin(egui::Margin::symmetric(12, 6)),
                )
                .show(ui, |ui| self.activity_panel(ui));
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::PAGE)
                    .inner_margin(egui::Margin::symmetric(
                        Self::PAGE_MARGIN_X as i8,
                        Self::PAGE_MARGIN_Y as i8,
                    )),
            )
            .show(ui, |ui| {
                // A page that scrolls is the page that says so. There is no wrapper here any
                // more: every page draws in the box it is given and scrolls its own columns
                // (Overview's story list, Articles' rows, Chat's transcript, the rail's long
                // lists). A wrap around this `match` made `ui.available_height()` unbounded,
                // so a page that measures its room — the Overview's map plate, which is the
                // width over the plate's own aspect ratio — was handed an infinite column
                // and painted a card 286 px past the bottom of a 900 px window. It also let
                // one wheel move two regions at once (Articles' rows and the window), and
                // let the Graph read a wheel as a zoom while the area under it scrolled.
                let page = self.page;
                match page {
                    Page::Overview => self.overview_page(ui),
                    Page::Articles => {
                        // A story opened from another page says where it came from, and the
                        // line is the click that goes back (the shell's business: the view
                        // does not know a Graph exists).
                        if self.back_row(ui) {
                            self.go_back();
                        }
                        // The row a click opened is brought into view on this frame: the
                        // request is one slot in the context's memory, written here and read
                        // by the list below.
                        if let Some(id) = self.reveal.take() {
                            crate::views::articles::reveal(ui.ctx(), id);
                        }
                        self.place_chip(ui);
                        // The card above the list is about **this page's scope** — the chip, the
                        // topic, the monitor, or the whole window — so a reader who narrowed to one
                        // country reads the sentence about that country, not the window's.
                        let ask = crate::views::brief::Ask::new(
                            self.brief_scope,
                            &self.brief_key,
                            &self.brief_note,
                            self.snapshot.hours,
                            self.briefing.as_ref(),
                            self.newest_story,
                        );
                        let opened = crate::views::articles::show_briefed(
                            ui,
                            &mut self.articles_view,
                            &self.snapshot.articles,
                            self.snapshot.hours,
                            Some(&ask),
                            Some(&self.queue),
                        );
                        self.open_story(opened);
                    }
                    Page::Graph => self.graph_page(ui),
                    Page::Reader => self.reader_page(ui),
                    Page::Topics => {
                        let stories = crate::views::topics::Stories {
                            topic: self.topic_open,
                            rows: &self.topic_stories,
                        };
                        let opened = crate::views::topics::show_with_queue(
                            ui,
                            &self.topics,
                            stories,
                            &self.queue,
                        );
                        self.open_story(opened);
                    }
                    Page::Monitors => {
                        let stories = match self.monitor_open {
                            Some(id) => crate::views::monitors::MonitorStories::of(
                                id,
                                &self.monitor_stories,
                            ),
                            None => crate::views::monitors::MonitorStories::none(),
                        };
                        let opened = crate::views::monitors::show_with_queue(
                            ui,
                            &self.monitors,
                            stories,
                            &self.queue,
                        );
                        self.open_story(opened);
                    }
                    Page::Chat => {
                        crate::views::chat::show(
                            ui,
                            &mut self.chat,
                            &self.model_url,
                            self.budget,
                        );
                    }
                    Page::Settings => {
                        // The engine the agent actually uses is whatever the archive's meta
                        // row says; the page shows the built-in one when nobody chose.
                        let engine = crate::views::settings::Engine::in_use(
                            self.search_engine.as_deref(),
                            crate::views::settings::BUILT_IN,
                        );
                        crate::views::settings::show_with_queue(ui, &engine, &self.queue);
                    }
                }
            });

        // The graph settles over ~2 s of frames; ask for them, then stop asking.
        if self.page == Page::Graph && self.graph.alpha > 0.005 {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        } else if self.asks_for_briefings {
            // In the live window, keep polling while the AI is working (fast for smooth
            // reasoning stream) or gently while idle so newly added topics and scheduled
            // runs appear without clicking Refresh.
            let wait = if self.activity.current().active {
                Duration::from_millis(400)
            } else {
                Duration::from_millis(1_500)
            };
            ui.ctx().request_repaint_after(wait);
        }
    }

    /// Poll `data/agent-status.json` and check if `atlas.db` / `-wal` changed on disk.
    fn poll_live_updates(&mut self) {
        if !self.asks_for_briefings && !self.activity_open {
            return;
        }
        let now = Instant::now();
        let active = self.activity.poll(now).active;
        if active && !self.activity_dismissed {
            self.activity_open = true;
        }
        if self.asks_for_briefings
            && now.saturating_duration_since(self.last_stamp_check) >= crate::activity::STAMP_POLL
        {
            self.last_stamp_check = now;
            let stamp = crate::activity::archive_stamp(&self.archive_path);
            if stamp.is_some() && stamp != self.archive_stamp {
                self.archive_stamp = stamp;
                self.dirty = true;
            }
        }
    }

    /// Bottom live AI activity bar + expandable Activity Log & Live Reasoning drawer.
    fn activity_panel(&mut self, ui: &mut egui::Ui) {
        let act = self.activity.current().clone();
        crate::activity::show(
            ui,
            &act,
            &mut self.activity_open,
            &mut self.activity_dismissed,
        );
    }

    /// The smallest plate the Graph page will draw, in whole [`theme::space::UNIT`]s: 64
    /// units tall (256 px), 80 wide (320 px). Below that the canvas is a strip and the panel
    /// beside it is unreadable, so a window this small overflows instead of shrinking the
    /// picture to nothing. A floor is the one number here that is not measured — everything
    /// above it comes from `ui.available_height()` and `ui.available_width()`.
    const MIN_PLATE_HEIGHT: f32 = theme::space::UNIT * 64.0;
    const MIN_PLATE_WIDTH: f32 = theme::space::UNIT * 80.0;

    /// Read the open row from the page's store — called once per frame, before the rail,
    /// so the read the rail may trigger already knows which row is open.
    fn follow_open_rows(&mut self, ctx: &egui::Context) {
        self.topic_open = crate::views::topics::open_topic(ctx);
        self.monitor_open = crate::views::monitors::open_monitor(ctx);
    }

    /// A story the reader asked for from any page: open it in the reading pane.
    ///
    /// One door for every list in the window — a dot, a topic's row, a monitor's row, a
    /// relation in the graph — because they all answer with the same thing.
    fn open_story(&mut self, story: Option<i64>) {
        if let Some(id) = story.filter(|id| *id > 0) {
            self.open_article(id);
        }
    }

    /// Open one story in the reader, from wherever the reader found it — a dot on the map, a
    /// node in the graph, a row under a topic.
    ///
    /// **A story is read on its own.** Measured with the shipped build: a country chip was
    /// clicked on the Overview (Germany), then a story was opened from the Graph ("Russia
    /// drops some of its pre-war demands…") and the Articles page came up showing the German
    /// list with the Russian story appended to the bottom of it — the page and the pane
    /// disagreed about what the reader had asked for, and nothing said why. So the click
    /// clears the three narrowing choices (a rollup chip, a topic row, a monitor row) and the
    /// search box, and the story is selected and scrolled to in the *whole* window. What was
    /// cleared is remembered in [`Origin`], and [`Self::go_back`] puts it back.
    fn open_article(&mut self, id: i64) {
        if id <= 0 {
            return;
        }
        // The origin is only remembered when the page actually changes: opening the next
        // story from the row beside this one is the same page, not a new place to return to.
        self.note_origin(Page::Articles);
        self.articles_view.selected = Some(id);
        // The row is asked for on the frame that opens it: the list is redrawn below in this
        // same frame and brings the story into view instead of leaving it below the fold.
        self.reveal = Some(id);
        self.page = Page::Articles;
        self.close_reader_unless_here();
        self.dirty = true;
    }

    /// Remember where the reader was, and clear the three choices a story is read without.
    ///
    /// One function for both destinations — the reading pane ([`Self::open_article`]) and the
    /// source page ([`Self::open_reader`]) — because the two must not drift: a reader who
    /// arrived at a page from a country chip and left it must find the chip in force, whether
    /// the thing they pressed was a row or that row's address.
    ///
    /// An origin is recorded only when the page **changes**. Pressing a row inside Articles
    /// used to record one too (`origin.is_none()` was enough), and the way-back row it turns on
    /// is painted *above* the page body — so the whole page dropped 32 px on the click frame,
    /// under the pointer that had just pressed the row: measured as the focus ring one row
    /// lower than the row it belongs to (`tests/search_filter.rs`,
    /// `the_focus_ring_covers_the_whole_row`, ring `y 299..366` against a title at `y 267`).
    /// Staying on a page is not a journey, and `Back` has nothing to go back to.
    fn note_origin(&mut self, destination: Page) {
        if self.page != destination {
            self.origin = Some(Origin {
                page: self.page,
                place: self.place.clone(),
                topic: self.topic_open,
                monitor: self.monitor_open,
                needle: self.articles_view.needle.clone(),
            });
        }
        self.place = None;
        self.topic_open = None;
        self.monitor_open = None;
        self.articles_view.needle.clear();
    }

    /// Open a story's **source page** in the window: the reader's link out of the archive.
    ///
    /// The address is gated by [`crate::link::openable`] — http(s), no credentials, printable
    /// ASCII — because this is the one call that hands a string to an engine that will fetch
    /// it. A build without an engine (`cfg` in `crate::webview`) draws the archive's copy of
    /// the page under a sentence that says which of the two the reader is looking at.
    ///
    /// The whole story travels with the address, not just the URL: the toolbar shows who filed
    /// it and when, and the text fallback needs the words. A URL the window knows nothing
    /// about is still openable — the engine fetches it — and reads as an untitled page.
    pub fn open_reader(&mut self, story: crate::views::reader::Story) {
        let Some(url) = crate::link::openable(&story.url) else {
            return;
        };
        self.note_origin(Page::Reader);
        // No bounds yet: the page is laid out by the frame that draws it, and `show` hands the
        // rectangle over on that frame (`views::reader::show`). An empty rectangle here is a
        // page that has not been placed, which the engine treats as nothing to paint.
        self.reader_page = crate::webview::open(url, egui::Rect::NOTHING);
        self.reader_story = Some(crate::views::reader::Story {
            url: url.to_string(),
            ..story
        });
        self.reader_stale = false;
        self.page = Page::Reader;
        // No read: the page draws from the archive the window already holds.
        self.dirty = false;
    }

    /// The archive row for an address, as the Reader's story — the row a "Read the source"
    /// link was drawn for.
    fn story_at(&self, url: &str) -> Option<crate::views::reader::Story> {
        self.snapshot
            .articles
            .iter()
            .chain(self.topic_stories.iter())
            .chain(self.monitor_stories.iter())
            .find(|article| article.url == url)
            .map(crate::views::reader::Story::of)
    }

    /// A "Read the source" press, from any page: the same door as [`Self::open_reader`], for a
    /// view that only had the address to give.
    fn open_source(&mut self, url: &str) {
        let Some(url) = crate::link::openable(url) else {
            return;
        };
        let story = self.story_at(url).unwrap_or_else(|| {
            crate::views::reader::Story {
                id: 0,
                title: String::new(),
                source: String::new(),
                published: String::new(),
                url: url.to_string(),
                text: String::new(),
                full_text: false,
            }
        });
        self.open_reader(story);
    }

    /// Return to the page the open story was found on, with the choices it had.
    fn go_back(&mut self) {
        let Some(origin) = self.origin.take() else {
            return;
        };
        self.place = origin.place;
        self.topic_open = origin.topic;
        self.monitor_open = origin.monitor;
        self.articles_view.needle = origin.needle;
        self.articles_view.selected = None;
        self.page = origin.page;
        self.close_reader_unless_here();
        self.dirty = true;
    }

    /// The one line a page shows when a story was opened from somewhere else: where back
    /// goes, and the key that does the same thing.
    ///
    /// Returns `true` on the frame the reader asked for it. Drawn by the shell rather than by
    /// the Articles page, because the page it returns to is the shell's business — the view
    /// does not know a Graph exists.
    fn back_row(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(origin) = self.origin.as_ref() else {
            return false;
        };
        let label = format!("Back to {}", origin.page.label());
        let mut clicked = false;
        ui.horizontal(|ui| {
            let button = egui::Button::new(theme::caption(&label)).frame(false);
            let response = ui.add(button).on_hover_text("the same as Alt+Left");
            // A frameless button throws away egui's own focus ring (`frame(false)` drops the
            // visuals the ring is drawn from: `style.rs:1386`, `theme/default_style.rs:73-83`).
            // The ring is painted here instead, at the theme's width and colour, so a keyboard
            // reader can see the way back without the row growing a box around it.
            if response.has_focus() {
                ui.painter().rect_stroke(
                    response.rect.expand(1.0),
                    egui::CornerRadius::ZERO,
                    theme::focus_stroke(),
                    egui::StrokeKind::Outside,
                );
            }
            if response.clicked() {
                clicked = true;
            }
            ui.add_space(theme::space::XS);
            ui.label(theme::dim("this story came from there"));
        });
        // The keyboard's way back, read where the row is drawn so it works on any page the
        // reader is on: `Alt+Left` is what every browser and every reader of one expects.
        if ui.input(|input| input.modifiers.alt && input.key_pressed(egui::Key::ArrowLeft)) {
            clicked = true;
        }
        clicked
    }

    /// The Reader: the story's own address, read in the window's engine.
    ///
    /// Three states, each said rather than implied. With an engine the page is the page and
    /// the view hands it the rectangle it paints in, so resizing the window moves it with the
    /// layout. Without one — a build for a platform with no engine, or an engine that refused
    /// to start — the archive's copy of the words is drawn under the sentence that says so
    /// ([`crate::views::reader::ENGINE_SENTENCE`]). With no story at all the page is an
    /// invitation, because the rail can be clicked before anything is open.
    ///
    /// The four answers are the shell's: `Back` and `Refresh` are the engine's own history and
    /// its own reload, `Ask` hands the story to Chat as a question, and `Tag` queues the page
    /// for the agent — which reads it, classifies it, and writes the labels. The window never
    /// writes the archive itself; a press is a request (`src/requests.rs`).
    fn reader_page(&mut self, ui: &mut egui::Ui) {
        // The line that goes back to the list this page was opened from — the shell's way out,
        // beside the engine's own arrow, which goes back in the *page's* history.
        if self.back_row(ui) {
            self.go_back();
            return;
        }
        let Some(story) = self.reader_story.clone() else {
            ui.add_space(theme::space::LG);
            ui.label(theme::dim(
                "No page is open. Press a story's address on Articles, Overview, Topics or \
                 Monitors and its page is read here, in this window.",
            ));
            return;
        };
        match crate::views::reader::show(ui, &story, self.reader_page.as_mut(), self.reader_stale) {
            crate::views::reader::Action::None => {}
            crate::views::reader::Action::Close => {
                // The page's own door out, beside the rail: the reader came from a page (a list,
                // a graph, a monitor) and goes back to it, or — when the address was opened
                // without a story behind it — to the list. `go_back` closes the view itself.
                if self.origin.is_some() {
                    self.go_back();
                } else {
                    self.set_page(Page::Articles);
                }
            }
            crate::views::reader::Action::Back => {
                if let Some(page) = self.reader_page.as_mut() {
                    page.back();
                }
            }
            crate::views::reader::Action::Refresh => {
                // A new engine page for the same address: the old one is dropped with its
                // history, which is what Reload means. The rectangle is the box this frame
                // gave the page, so nothing is painted outside it for a frame.
                self.reader_page = crate::webview::open(&story.url, ui.max_rect());
                ui.ctx().request_repaint();
            }
            crate::views::reader::Action::Ask => {
                self.chat.draft(question_about(&story));
                self.page = Page::Chat;
            }
            crate::views::reader::Action::Tag => {
                // The agent reads and classifies; the window only says which page. A failure
                // to queue is a file the reader can fix, and the status row already names the
                // queue — nothing is echoed here.
                let _ = crate::requests::push_to(
                    &self.queue,
                    "research",
                    &crate::requests::research_payload(&story.url, 2),
                );
            }
        }
    }

    /// Ask the agent for the briefing of the scope the window is showing, if it has none.
    ///
    /// This is the one thing the window asks for without a press, and it is deliberate: the
    /// briefing is the answer to "what is there?", which is the question a click on a country or
    /// a topic *is*. The window writes no words and decides no content — it names the scope the
    /// reader is looking at, and the agent's model writes the sentence (`atlas-ingest`).
    ///
    /// Called once per frame and not from [`Atlas::reload`], because the *read* is not the event
    /// that matters: a window that opened before any read happened (the shipped one reads in its
    /// constructor, where this switch is still off) would otherwise never ask until the reader
    /// changed the window. The call is a `HashSet` lookup unless the scope is new, and
    /// `brief_asked` is what keeps it to one ask — a page read twice, or a chip visited again,
    /// draws the row the first ask produced instead of queueing a second.
    fn maybe_ask_for_briefing(&mut self) {
        if !self.asks_for_briefings || self.briefing.is_some() || self.snapshot.articles.is_empty()
        {
            return;
        }
        let slot = (
            self.brief_scope.to_string(),
            self.brief_key.clone(),
            self.snapshot.hours,
        );
        if !self.brief_asked.insert(slot) {
            return;
        }
        let why = format!(
            "the page about {} has stories and no briefing yet",
            self.brief_note
        );
        let payload = crate::views::brief::payload(
            self.brief_scope,
            &self.brief_key,
            self.snapshot.hours,
            &why,
        );
        let _ = crate::requests::push_to(&self.queue, crate::views::brief::KIND, &payload);
    }

    /// The Overview: the map card, then the column of stories and counts below it.
    ///
    /// Both doors answer with something to open: a story (a dot, a row) opens that story in
    /// the reader, and a place (a chip on the rollup) narrows the list to that place. The
    /// page draws its own column and scrolls it, so this is deliberately not wrapped.
    /// The terms screen: draw it, and act on the one answer it can give.
    ///
    /// Kept here rather than inside the view because the *decision* is the shell's: the view
    /// says "accepted" or "quit", and this is what writes the record, closes the gate and, on
    /// a failure, tells the reader in the view's own voice that nothing was written. A screen
    /// that answered `false` to a write it could not make would be a screen that lied about
    /// the only thing it exists to do.
    fn draw_gate(&mut self, ui: &mut egui::Ui) {
        let Some(mut gate) = self.gate.take() else {
            return;
        };
        let frame = egui::Frame::NONE
            .fill(theme::PAGE)
            .inner_margin(egui::Margin::same(0));
        let choice = frame
            .show(ui, |ui| crate::views::license::show(ui, &mut gate))
            .inner;
        match choice {
            Some(crate::views::license::Choice::Accept) => {
                match crate::requests::accept_at(&self.queue, crate::views::license::VERSION) {
                    Ok(()) => {
                        // The gate stays down for the rest of the session: the row is on
                        // disk, and the first workspace frame is drawn next.
                        self.dirty = true;
                        ui.ctx().request_repaint();
                    }
                    Err(failure) => {
                        gate.failure = Some(failure);
                        self.gate = Some(gate);
                    }
                }
            }
            Some(crate::views::license::Choice::Quit) => {
                ui.ctx().send_viewport_cmd(Viewport::Close);
                self.gate = Some(gate);
            }
            None => self.gate = Some(gate),
        }
    }

    /// Is the terms screen still up? A test's question, and the one the status row would ask.
    pub fn gate_is_up(&self) -> bool {
        self.gate.is_some()
    }

    fn overview_page(&mut self, ui: &mut egui::Ui) {
        // The Overview's briefing is the whole window's: the page is the largest question the
        // window asks, so the card above its column is the one sentence about everything the
        // archive holds in this window. A stale card is still shown — it says so.
        // One constructor for both cards (`Ask::new` derives staleness from the archive's
        // newest id), so the two pages cannot disagree about what "stale" means.
        let ask = crate::views::brief::Ask::new(
            "window",
            "",
            "the window",
            self.snapshot.hours,
            self.window_briefing.as_ref(),
            self.newest_story,
        );
        let choice = crate::views::overview::show_briefed(
            ui,
            &mut self.overview,
            &self.snapshot,
            Some(&ask),
            Some(&self.queue),
        );
        if let Some(id) = choice.story {
            self.open_article(id);
        }
        // A rollup chip: the page says *what kind* of word it counted ([`Door`]), and the
        // window opens exactly that list. A chip whose word and kind disagree is not acted on
        // at all rather than opening an empty page — [`crate::views::overview::Choice::door`]
        // answers `None` for it, so a broken chip is a chip that does nothing, never one that
        // lies about a count.
        if let Some(door) = choice.door() {
            // The same click also clears the search box: the reader asked for a country or a
            // place, and a leftover needle would hide the very stories the chip promises.
            self.articles_view.needle.clear();
            // …and it lets go of the story the pane was showing. The reader moved from one
            // story to a rollup list, and keeping that story would put a row in the list that
            // the chip did not count — the strip's number and the list would disagree by one.
            // The pane says "Choose a story on the left to read it here." instead.
            self.articles_view.selected = None;
            self.narrow_to(door);
        }
    }

    /// Narrow the list to a rollup row, exactly as a chip under the map does.
    ///
    /// Public for the reason [`Self::set_page`] and [`Self::open_reader`] are: a test of the
    /// *window* has to be able to open a country's list without hunting for a chip's pixels,
    /// and this is the one line a chip's press runs — the page's own value ([`Door`]: a word and
    /// what kind of word it is), never a bare string, so a test cannot narrow to something no
    /// chip counts. The page switch is part of the door: a chip that narrowed the list while the
    /// Overview stayed up would be a click that visibly did nothing.
    pub fn narrow_to(&mut self, door: Door) {
        self.place = Some(door);
        self.page = Page::Articles;
        self.close_reader_unless_here();
        self.dirty = true;
    }

    /// A strip above the Articles list naming the rollup row the reader chose, with the
    /// button that gives the whole window back. Drawn here rather than in the view so the
    /// page stays a reader of rows and the shell stays the owner of "what is being shown".
    ///
    /// The count in this line is `self.snapshot.articles.len()` — the list actually on screen
    /// — so the strip cannot claim a story the list below it does not hold.
    fn place_chip(&mut self, ui: &mut egui::Ui) {
        let Some(door) = self.place.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            // The number first: it is the list's own length, always — and when the chip
            // counted more than `LIST_ROWS` the line says `400 of 1 050 from USA` instead of
            // printing a number over a shorter list. A strip that prints the chip's number
            // over a list that holds fewer rows is the same defect as a chip that opens
            // nothing, one step further from the archive.
            let shown = self.snapshot.articles.len() as i64;
            let said = if door.count > shown {
                format!("{} of {} from {}", nf(shown), nf(door.count), door.word)
            } else {
                format!("{} from {}", nf(shown), door.word)
            };
            ui.label(theme::body(&said));
            if ui
                .add(egui::Button::new(theme::dim("Show everything")).frame(false))
                .clicked()
            {
                self.place = None;
                self.dirty = true;
            }
        });
        ui.add_space(theme::space::SM);
    }

    /// The Graph page: a header, then the plate and the relations column, both sized from
    /// the box the header left and laid out by [`GraphPlan`] — side by side when the window
    /// can hold the plate's floor, the gap and the column, and stacked when it cannot.
    ///
    /// Every number here is measured from `ui`; the one typed decision is the plate's floor.
    /// This page is deliberately **not** inside a [`egui::ScrollArea`]: the plate reads the
    /// wheel to zoom (`graph.rs` `smooth_scroll_delta`), and a scroll area around it would
    /// move the canvas while the reader zoomed.
    fn graph_page(&mut self, ui: &mut egui::Ui) {
        ui.label(theme::page_title("Graph"));
        ui.label(theme::dim(&format!(
            "{} entities and {} relations in the window — click a node",
            nf(self.graph.nodes.len() as i64),
            nf(self.graph.edges.len() as i64)
        )));
        ui.add_space(theme::space::MD);
        // The plan's gap is the gap the widgets get: `item_spacing` is set from the same
        // constant, so the rects the test asserts are the rects the two parts are given.
        ui.spacing_mut().item_spacing = egui::vec2(GRAPH_GAP, GRAPH_GAP);
        let plan = GraphPlan::from_box(ui.available_rect_before_wrap());
        // Both parts answer with the story a reader asked for: the plate when a node's
        // stories are opened from it, the panel when a relation row is clicked. The page
        // used to drop both answers — a click in the relation panel then left the reader on
        // the Graph page, which is the opposite of what `graph.rs`'s panel documents.
        let mut opened: Option<i64> = None;
        if plan.stacked {
            // Plate on top, column below, both the full width of the page box and neither
            // of them off the edge: the column used to be drawn after a 320 px plate, which
            // put its text past the window's right edge on a narrow window.
            opened = ui
                .allocate_ui(plan.plate.size(), |ui| {
                    self.graph.show(ui, plan.plate.height())
                })
                .inner;
            if let Some(id) = ui
                .allocate_ui(plan.column.size(), |ui| graph::panel(ui, &self.graph))
                .inner
            {
                opened = Some(id);
            }
        } else {
            ui.horizontal_top(|ui| {
                opened = ui
                    .allocate_ui(plan.plate.size(), |ui| {
                        self.graph.show(ui, plan.plate.height())
                    })
                    .inner;
                if let Some(id) = ui
                    .allocate_ui(plan.column.size(), |ui| graph::panel(ui, &self.graph))
                    .inner
                {
                    opened = Some(id);
                }
            });
        }
        if let Some(id) = opened {
            self.open_article(id);
        }
    }
}

/// The gap between the plate and the relations column, and between the stacked pair.
pub const GRAPH_GAP: f32 = theme::space::LG;

/// Where the Graph page puts its two parts, from the box the header left.
///
/// Kept out of [`Atlas::graph_page`] so the decision is assertable without a window, and
/// used by it so the decision is what gets drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GraphPlan {
    plate: egui::Rect,
    column: egui::Rect,
    stacked: bool,
}

impl GraphPlan {
    fn from_box(page: egui::Rect) -> GraphPlan {
        let width = page.width().max(1.0);
        let height = page.height().max(1.0);
        // Side by side needs three widths at once: the plate's floor, the gap, the column.
        // Below that, plate + column cannot both be inside the page box, and the old code
        // proved it by drawing the column past the window's edge.
        let side_by_side = width >= Atlas::MIN_PLATE_WIDTH + GRAPH_GAP + graph::PANEL_WIDTH;
        if side_by_side {
            let plate_width = width - GRAPH_GAP - graph::PANEL_WIDTH;
            let tall = height.max(Atlas::MIN_PLATE_HEIGHT);
            GraphPlan {
                plate: egui::Rect::from_min_size(page.min, egui::vec2(plate_width, tall)),
                column: egui::Rect::from_min_size(
                    egui::pos2(page.min.x + plate_width + GRAPH_GAP, page.min.y),
                    egui::vec2(graph::PANEL_WIDTH, tall),
                ),
                stacked: false,
            }
        } else {
            // Stacked: one height budget for both, the plate taking [`Atlas::PLATE_SHARE`]
            // of it (and never less than its floor, when the budget can afford that), the
            // column the rest. Nothing overflows the page box in either direction.
            let shared = (height - GRAPH_GAP).max(1.0);
            let plate_height = (shared * Atlas::PLATE_SHARE)
                .max(Atlas::MIN_PLATE_HEIGHT)
                .min(shared);
            let column_height = (shared - plate_height).max(1.0);
            GraphPlan {
                plate: egui::Rect::from_min_size(page.min, egui::vec2(width, plate_height)),
                column: egui::Rect::from_min_size(
                    egui::pos2(page.min.x, page.min.y + plate_height + GRAPH_GAP),
                    egui::vec2(width, column_height),
                ),
                stacked: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_row_is_part_of_the_demand_and_the_page_is_not() {
        // The shell reads an open row's stories once, when the row opens: the row is part of
        // what was asked for, so the same row must not cause a second read and a different
        // one must.
        //
        // **This test used to assert the opposite about the page** — that a row counted "only
        // on the pages that have rows" — and that is exactly what made Overview → Graph a
        // read, which ends in `GraphView::fill` and re-frames the plate over the reader's
        // camera. The page is not part of a demand any more (see [`Demand`]); the row still
        // belongs to its own page, because `topic` and `monitor` are two fields: a topic that
        // is open can never be read as a monitor's row, whatever page is showing.
        let topics = Demand::of(72, "", None, Some(4), None);
        assert_eq!(topics.topic, Some(4));
        assert_eq!(topics.monitor, None);
        assert!(
            !needs_read(Some(&topics), &topics, false),
            "the same open topic is the same demand"
        );
        assert!(
            needs_read(Some(&topics), &Demand::of(72, "", None, Some(5), None), false),
            "another row is another read"
        );
        assert!(
            needs_read(Some(&topics), &Demand::of(72, "", None, None, None), false),
            "closing the row is a read: the stories under it are gone"
        );
        assert!(
            needs_read(Some(&topics), &Demand::of(72, "", None, None, Some(4)), false),
            "a topic id is not a monitor id, even when the two numbers are equal"
        );
        assert_eq!(
            Demand::of(72, "", None, Some(0), Some(-1)),
            Demand::of(72, "", None, None, None),
            "a zero id is not a row"
        );
    }

    #[test]
    fn a_page_switch_reads_nothing_and_keeps_the_camera_where_the_reader_left_it() {
        // Measured by `tests/layout_screens.rs` before this change: after a wheel and a drag
        // the plate sat 92.16 px from a fresh window's picture, and after Overview → Graph it
        // was 0.00 px from the fresh picture — the gesture was gone, because the page was part
        // of the demand and every read ends in `GraphView::fill`, which frames the plate with
        // zoom 1 and pan 0.
        //
        // No archive is opened: what is under test is the frame's bookkeeping, and a window
        // with no file at its path only records the demand it asked for — it issues no query
        // either way, so what is counted here is the shell's own decision to read.
        let mut atlas = Atlas::with_archive(PathBuf::from("/nonexistent/atlas.db"));
        let built = atlas.reads;
        assert_eq!(built, 1, "the window reads once while it is being built");
        for page in [
            Page::Graph,
            Page::Articles,
            Page::Topics,
            Page::Monitors,
            Page::Chat,
            Page::Overview,
            Page::Graph,
        ] {
            atlas.set_page(page);
            atlas.sync();
            assert_eq!(
                atlas.reads, built,
                "switching to {page:?} read the archive again"
            );
        }
        // …and the camera the switch did not touch is still the reader's.
        let (zoom, pan) = (2.5f32, egui::vec2(-120.0, 40.0));
        atlas.graph.zoom = zoom;
        atlas.graph.pan = pan;
        for page in [Page::Overview, Page::Graph] {
            atlas.set_page(page);
            atlas.sync();
        }
        assert_eq!(atlas.graph.zoom, zoom, "a page switch re-framed the plate");
        assert_eq!(atlas.graph.pan, pan, "a page switch moved the plate");
    }

    #[test]
    fn a_frame_with_the_same_demand_reads_nothing() {
        // The rule the whole change exists for: an unchanged demand is not a read, so a
        // repaint (a mouse move, a scroll, the graph's own animation) costs no query.
        let loaded = Demand::of(72, "chip exports", None, None, None);
        assert!(!needs_read(Some(&loaded), &loaded, false));
        assert!(
            !needs_read(Some(&loaded), &Demand::of(72, "chip exports", None, None, None), false),
            "a second equal demand is the same demand"
        );
    }

    #[test]
    fn the_drawing_path_never_copies_the_loaded_rows_again() {
        // The bug this file was rewritten for: every frame built a `Snapshot { pins:
        // self.pins.clone(), articles: …, countries: … }`, and every read cloned the
        // entities and edges into the graph. A test cannot open a window here, so it reads
        // the drawing half of this file — the same technique `tests/query_budget.rs` uses
        // for the limits — and refuses a copy of the loaded rows in it.
        const SOURCE: &str = include_str!("app.rs");
        let drawing = SOURCE
            .split("    fn rail(")
            .nth(1)
            .expect("the rail is where drawing starts");
        let drawing = drawing.split("#[cfg(test)]").next().unwrap_or(drawing);
        assert!(
            !drawing.contains("Snapshot {"),
            "a snapshot is built while drawing: that is the per-frame clone this file exists \
             without"
        );
        for copied in [
            ".pins.clone()",
            ".countries.clone()",
            ".articles.clone()",
            ".entities.clone()",
            ".edges.clone()",
            ".topics.clone()",
            ".monitors.clone()",
        ] {
            assert!(
                !drawing.contains(copied),
                "the frame copies loaded rows ({copied}); the views take a reference"
            );
        }
        // And the page that needs a snapshot is handed the one this struct holds.
        assert!(
            drawing.contains("&self.snapshot"),
            "the Overview is not handed the loaded snapshot"
        );
        assert!(
            drawing.contains("&self.snapshot.articles"),
            "the article list is not handed the loaded rows"
        );
    }

    #[test]
    fn a_changed_window_search_or_row_reads_again() {
        // What still costs a query. **The page is not in this list and must not be**: it
        // decides what is drawn, and a page switch that read the archive would re-frame the
        // graph over the reader's camera (`a_page_switch_reads_nothing_and_keeps_the_camera_…`
        // asserts the other half of that rule).
        let loaded = Demand::of(72, "", None, None, None);
        for wanted in [
            Demand::of(24, "", None, None, None),
            Demand::of(168, "", None, None, None),
            Demand::of(72, "chip", None, None, None),
            Demand::of(72, "", None, Some(4), None),
            Demand::of(72, "", None, None, Some(2)),
        ] {
            assert!(
                needs_read(Some(&loaded), &wanted, false),
                "{:?} is a new demand and must be read",
                wanted
            );
        }
    }

    #[test]
    fn refresh_reads_even_when_nothing_changed() {
        let loaded = Demand::of(72, "", None, None, None);
        assert!(needs_read(Some(&loaded), &loaded, true));
    }

    #[test]
    fn the_first_frame_has_nothing_to_show_and_reads() {
        assert!(needs_read(None, &Demand::of(72, "", None, None, None), false));
    }

    #[test]
    fn the_search_key_ignores_case_and_surrounding_space() {
        // `Archive::articles` matches case-insensitively, so "Chip " is the query
        // "chip": a read for the difference would be a read for nothing.
        assert_eq!(needle_key("  Chip Exports "), "chip exports");
        assert_eq!(needle_key("chip exports"), "chip exports");
        assert_eq!(
            Demand::of(72, "Chip Exports ", None, None, None),
            Demand::of(72, " chip exports", None, None, None)
        );
        // And a real change is still a change.
        assert_ne!(needle_key("chip"), needle_key("chips"));
    }

    #[test]
    fn the_rail_lists_every_page_once_and_chat_is_one_of_them() {
        // The count is a tripwire, not a fact about the design: it is bumped **by hand** the
        // day a page joins the rail, so that adding a variant to `Page` without listing it in
        // `PAGES` (or listing it twice) cannot pass unnoticed. Eight since `Reader` joined
        // (`Page::Reader`, the story's own page in the window's engine); the rest of this test
        // is what proves the list is complete and worded as the rail needs it.
        assert_eq!(PAGES.len(), 8);
        assert!(PAGES.contains(&Page::Chat));
        assert!(PAGES.contains(&Page::Reader));
        assert_eq!(Page::Chat.label(), "Chat");
        assert_eq!(Page::Reader.label(), "Reader");
        for (index, page) in PAGES.iter().enumerate() {
            assert!(
                !PAGES[..index].contains(page),
                "{} is listed twice",
                page.label()
            );
            let label = page.label();
            assert!(
                label.chars().next().is_some_and(|first| first.is_uppercase())
                    && !label.contains(' '),
                "{:?} is not a one-word sentence-case label",
                label
            );
        }
    }

    #[test]
    fn the_archive_is_named_without_the_machine_around_it() {
        // A row is one line, and a path is a home directory plus whatever the reader's
        // directory is called: `data/atlas.db` is what a person can act on, and the label
        // is built from components so no absolute prefix can arrive with it.
        let root = crate::assets::root();
        assert_eq!(
            crate::assets::label(&root.join("data").join("atlas.db")),
            "data/atlas.db"
        );
        // A file outside the checkout is named by its own name, not by the way to it.
        assert_eq!(crate::assets::label(std::path::Path::new("/home/somebody/atlas.db")), "atlas.db");
        let label = crate::assets::label(&crate::assets::archive());
        assert!(!label.starts_with('/'), "{:?} is an absolute path", label);
        assert!(!label.contains('\\'), "{:?} carries a separator", label);
        assert!(!label.contains('\n'), "{:?} is more than one line", label);
        assert!(label.ends_with("atlas.db"), "{:?}", label);
    }

    #[test]
    fn the_status_row_names_the_model_without_quoting_the_setting() {
        // `ATLAS_MODEL_URL` can be `http://alice:secret@host` or carry a newline, so the
        // row's words are fixed and the address is only ever dialled by the Chat view —
        // where `http::endpoint` refuses a bad one without repeating it.
        assert!(!CHAT_NEEDS_MODEL.contains("://"), "{:?}", CHAT_NEEDS_MODEL);
        assert!(
            !CHAT_NEEDS_MODEL.chars().any(char::is_control),
            "{:?}",
            CHAT_NEEDS_MODEL
        );
        assert_eq!(CHAT_NEEDS_MODEL, "chat needs the local model");
    }

    #[test]
    fn a_read_faster_than_a_millisecond_says_so() {
        assert_eq!(millis(Duration::from_micros(200)), "<1");
        assert_eq!(millis(Duration::from_millis(12)), "12");
        assert_eq!(millis(Duration::ZERO), "<1");
    }

    #[test]
    fn the_chat_page_is_told_where_the_model_is() {
        // Three sources, in the order that makes Chat work out of the box: the reader's
        // `ATLAS_MODEL_URL`, the port `data/config.json` names (where `atlas start` puts
        // the server), then this window's documented default.
        assert_eq!(model_url_from(None, None), DEFAULT_MODEL_URL);
        assert_eq!(model_url_from(Some(""), None), DEFAULT_MODEL_URL);
        assert_eq!(model_url_from(Some("   "), None), DEFAULT_MODEL_URL);
        assert_eq!(model_url_from(None, Some(8081)), "http://127.0.0.1:8081");
        // The setting wins over the configured port, and is trimmed but not rewritten:
        // `http::endpoint` is the one place an address is judged, and the Chat view calls it.
        assert_eq!(
            model_url_from(Some(" http://127.0.0.1:9091 "), Some(8081)),
            "http://127.0.0.1:9091"
        );
        // What this answers on its own is always a loopback literal this window built.
        for url in [model_url_from(None, None), model_url_from(None, Some(8081))] {
            assert!(url.starts_with("http://127.0.0.1:"), "{}", url);
        }
    }

    #[test]
    fn the_chat_page_is_told_the_archive_and_the_window_the_rail_shows() {
        // Chat used to read the archive over a hard-coded 72 h: `ChatState::set_window`
        // existed and nothing called it. `reload` is where the demand — hours included —
        // is settled, so that is the one place this is said. The path need not exist: the
        // wiring is about what Chat is *told*, and a window with no archive still opens
        // and says so in the status row.
        let path = std::env::temp_dir().join("atlas-desktop-chat-window.db");
        let mut atlas = Atlas::with_archive(path.clone());
        assert_eq!(
            atlas.chat.window(),
            Some((path.as_path(), 72)),
            "the rail's default window, over this window's own archive"
        );
        // A rail that changes the hours reaches Chat through the same field and the same
        // `sync`: no second rule to keep in step.
        atlas.hours = 168;
        atlas.sync();
        assert_eq!(atlas.chat.window(), Some((path.as_path(), 168)));
    }

    /// What one frame of the window paints, measured from its shapes.
    ///
    /// Measured rather than read off a field, because both defects were geometry: a literal
    /// 620 px canvas in a 1400 px window, and a relations column drawn *past* the window's
    /// right edge. A field would have had to be added to notice either.
    struct Painted {
        /// The central panel's fill — the page's surface, margins included.
        panel: egui::Rect,
        /// The rail's surface: the narrower of the two [`theme::PAGE`] rectangles.
        rail: egui::Rect,
        /// The page box: `panel` inside its inner margin, which is the box the pages get and
        /// the box the audit reports (a 700 px window with the full rail was 473 × 450).
        page_box: egui::Rect,
        /// The graph's plate: the largest filled grey rectangle that is neither the page nor
        /// a panel, painted with `graph.rs`'s own plate fill.
        plate: egui::Rect,
        /// Every text run and every filled rectangle that is not a page/panel surface, in
        /// window coordinates — what the page actually paints.
        runs: Vec<egui::Rect>,
        /// The strings those runs draw, with where they sit and the clip they were painted
        /// under — so a test can ask what a reader would read in the rail, what on the page,
        /// and whether a run stayed where it was allowed to be drawn.
        texts: Vec<(egui::Rect, egui::Rect, String)>,
        /// The ink each of those strings was painted in.
        ///
        /// The rail marks the window in force by ink alone (there is no fill), so "the chip
        /// that is on is visibly the one that is on" is a question only the colour can
        /// answer: a test that read `self.hours` would pass with a rail that never showed it.
        inks: Vec<(egui::Rect, egui::Color32)>,
    }

    impl Painted {
        fn measure(shapes: Vec<egui::epaint::ClippedShape>) -> Painted {
            fn walk(
                shape: &egui::epaint::Shape,
                clip: egui::Rect,
                out: &mut Vec<(egui::Rect, Option<egui::Color32>)>,
                texts: &mut Vec<(egui::Rect, egui::Rect, String)>,
                inks: &mut Vec<(egui::Rect, egui::Color32)>,
            ) {
                match shape {
                    egui::epaint::Shape::Rect(rect) => out.push((rect.rect, Some(rect.fill))),
                    egui::epaint::Shape::Text(text) => {
                        let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                        out.push((rect, None));
                        texts.push((rect, clip, text.galley.text().to_owned()));
                        if let Some(section) = text.galley.job.sections.first() {
                            inks.push((rect, section.format.color));
                        }
                    }
                    egui::epaint::Shape::Vec(items) => {
                        for item in items {
                            walk(item, clip, out, texts, inks);
                        }
                    }
                    _ => {}
                }
            }
            let mut found = Vec::new();
            let mut texts = Vec::new();
            let mut inks = Vec::new();
            for clipped in &shapes {
                walk(
                    &clipped.shape,
                    clipped.clip_rect,
                    &mut found,
                    &mut texts,
                    &mut inks,
                );
            }
            let mut panels: Vec<egui::Rect> = Vec::new();
            let mut plate = (0.0, egui::Rect::NOTHING);
            let mut runs = Vec::new();
            for (rect, fill) in found {
                let area = rect.width() * rect.height();
                match fill {
                    Some(fill) if fill == theme::PAGE => panels.push(rect),
                    Some(fill) if fill == theme::PANEL => {}
                    Some(fill) => {
                        let grey = fill.r() == fill.g() && fill.g() == fill.b();
                        if grey && area > plate.0 {
                            plate = (area, rect);
                        }
                        if area > 4.0 {
                            runs.push(rect);
                        }
                    }
                    None => runs.push(rect),
                }
            }
            // The rail and the page are the two `PAGE` surfaces; the narrow one is the rail.
            let rail = panels
                .iter()
                .copied()
                .min_by(|a, b| a.width().total_cmp(&b.width()))
                .unwrap_or(egui::Rect::NOTHING);
            let panel = panels
                .into_iter()
                .max_by(|a, b| (a.width() * a.height()).total_cmp(&(b.width() * b.height())))
                .unwrap_or(egui::Rect::NOTHING);
            let page_box = panel.shrink2(egui::vec2(
                Atlas::PAGE_MARGIN_X,
                Atlas::PAGE_MARGIN_Y,
            ));
            Painted {
                panel,
                rail,
                page_box,
                plate: plate.1,
                runs,
                texts,
                inks,
            }
        }

        /// The runs painted in the rail — left of the page box.
        fn in_the_rail(&self) -> Vec<egui::Rect> {
            self.runs
                .iter()
                .copied()
                .filter(|run| run.left() < self.page_box.left() && run.top() < self.page_box.bottom())
                .collect()
        }

        /// The strings painted inside the rail — what a reader sees there, page title
        /// included only when the rail is what paints it.
        fn rail_texts(&self) -> Vec<&str> {
            self.texts
                .iter()
                .filter(|(rect, _, _)| self.rail.contains(rect.center()))
                .map(|(_, _, text)| text.as_str())
                .collect()
        }

        /// Where the rail paints `label`, if it paints it at all.
        fn rail_run(&self, label: &str) -> Option<egui::Pos2> {
            self.texts
                .iter()
                .find(|(rect, _, text)| text == label && self.rail.contains(rect.center()))
                .map(|(rect, _, _)| rect.center())
        }

        /// The ink the rail paints `label` in, if it paints it at all.
        ///
        /// The rail has no fill for the window in force: INK is the window that is on, DIM
        /// the two that are not. So a test that wants "the chip in force is visibly the
        /// current one" has to read the colour, not the string.
        fn rail_ink(&self, label: &str) -> Option<egui::Color32> {
            self.inks
                .iter()
                .find(|(rect, _)| {
                    self.rail.contains(rect.center())
                        && self
                            .texts
                            .iter()
                            .any(|(run, _, text)| run == rect && text == label)
                })
                .map(|(_, ink)| *ink)
        }

        /// Where the *page* paints `label`, if it paints it at all — the door a pointer test
        /// clicks. Same rule as [`Self::rail_run`]: the pixel is the box the page drew that
        /// string in, not a coordinate the test guessed.
        fn page_run(&self, label: &str) -> Option<egui::Pos2> {
            self.texts
                .iter()
                .find(|(rect, _, text)| text == label && self.page_box.contains(rect.center()))
                .map(|(rect, _, _)| rect.center())
        }

        /// The strings the page painted inside its own box — what a reader reads there, used
        /// to ask whether a strip says what it counted.
        fn page_texts(&self) -> Vec<&str> {
            self.texts
                .iter()
                .filter(|(rect, _, _)| self.page_box.contains(rect.center()))
                .map(|(_, _, text)| text.as_str())
                .collect()
        }

        /// Every text run and the clip it was painted under, so a test can ask whether a run
        /// stayed inside what the window let it draw.
        fn clipped_texts(&self) -> Vec<(egui::Rect, egui::Rect, &str)> {
            self.texts
                .iter()
                .map(|(rect, clip, text)| (*rect, *clip, text.as_str()))
                .collect()
        }

        /// The runs the page paints inside its own box — the graph's column, its labels, its
        /// rows. The rail's text and the status row are outside [`Self::page_box`] and are
        /// not part of the page.
        fn on_the_page(&self) -> Vec<egui::Rect> {
            self.runs
                .iter()
                .copied()
                .filter(|run| self.page_box.intersects(*run))
                .collect()
        }
    }

    /// One frame of the real window at `width × height`, on `page`.
    fn painted_at(
        width: f32,
        height: f32,
        page: Page,
        rail_choice: Option<bool>,
        events: Vec<egui::Event>,
    ) -> Painted {
        let path = std::env::temp_dir().join("atlas-desktop-layout.db");
        let mut atlas = Atlas::with_archive(path);
        atlas.page = page;
        // `None` is "the window decides", which is the shipping behaviour.
        atlas.rail_choice = rail_choice;
        let context = egui::Context::default();
        draw_frame(&context, &mut atlas, width, height, events)
    }

    /// One frame of an existing window, at `width × height`, with the input events it is
    /// given: the shape a pointer test needs, since a click arrives as a press frame and a
    /// release frame.
    fn draw_frame(
        context: &egui::Context,
        atlas: &mut Atlas,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
    ) -> Painted {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            events,
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| atlas.draw(ui));
        let shapes = std::mem::take(&mut output.shapes);
        // 0.37 asserts on a `FullOutput` dropped with texture deltas still in it.
        output.drop_without_applying_deltas();
        Painted::measure(shapes)
    }

    /// One click on a control the rail paints, from a given page and window; answers what
    /// the window says afterwards — the page, the window, and the read count before and
    /// after, so a door that re-reads can be told from one that does not.
    ///
    /// Three frames: the press, the release, and one quiet frame so the effect of the click
    /// (a page change is part of the next demand, Refresh sets the dirty flag) has landed
    /// before anything is read back.
    fn click_rail(
        width: f32,
        height: f32,
        start: (Page, i64),
        label: &str,
    ) -> (Page, i64, u32, u32) {
        let path = std::env::temp_dir().join("atlas-desktop-rail-clicks.db");
        let mut atlas = Atlas::with_archive(path);
        atlas.page = start.0;
        atlas.hours = start.1;
        let context = egui::Context::default();
        let first = draw_frame(&context, &mut atlas, width, height, Vec::new());
        let reads_before = atlas.reads;
        let at = first.rail_run(label).unwrap_or_else(|| {
            panic!(
                "the rail paints no `{label}`; it paints {:?}",
                first.rail_texts()
            )
        });
        for pressed in [true, false] {
            draw_frame(
                &context,
                &mut atlas,
                width,
                height,
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
        draw_frame(&context, &mut atlas, width, height, Vec::new());
        (atlas.page, atlas.hours, reads_before, atlas.reads)
    }

    /// One press of `key` with the platform's command modifier held.
    fn command_key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }
    }

    #[test]
    fn the_plate_fills_the_box_the_header_leaves_on_a_window_wide_enough_to_sit_beside() {
        // The report: "insbesondere beim graph, er ist nicht mal full screen". Before this
        // change the canvas was `allocate_ui(vec2(width, 620.0))` inside the window's own
        // scroll area: 620 px tall in *every* window, and it scrolled. Measured at 1200 px,
        // which is wide enough for the column to sit beside the plate.
        let short = (1200.0_f32, 1000.0_f32);
        let tall = (1200.0_f32, 1400.0_f32);
        let short_frame = painted_at(short.0, short.1, Page::Graph, None, Vec::new());
        let tall_frame = painted_at(tall.0, tall.1, Page::Graph, None, Vec::new());
        let (short_plate, tall_plate) = (short_frame.plate, tall_frame.plate);
        println!(
            "graph plate: {:.0}x{:.0} at {:.0}x{:.0} (page box {:.0}x{:.0}); \
             {:.0}x{:.0} at {:.0}x{:.0} (page box {:.0}x{:.0})",
            short_plate.width(), short_plate.height(), short.0, short.1,
            short_frame.page_box.width(), short_frame.page_box.height(),
            tall_plate.width(), tall_plate.height(), tall.0, tall.1,
            tall_frame.page_box.width(), tall_frame.page_box.height(),
        );

        for (frame, window) in [(&short_frame, short), (&tall_frame, tall)] {
            let plate = frame.plate;
            // 1. Inside the page box, and reaching its foot: what is below it is the status
            //    row and the page's bottom margin.
            assert!(
                plate.left() >= frame.page_box.left() - 1.0
                    && plate.right() <= frame.page_box.right() + 1.0
                    && plate.top() >= frame.page_box.top() - 1.0
                    && plate.bottom() <= frame.page_box.bottom() + 1.0,
                "the plate {plate:?} is outside the page box {:?}",
                frame.page_box
            );
            let below = frame.page_box.bottom() - plate.bottom();
            assert!(
                below.abs() <= 1.0,
                "the plate stops {below:.0} px above the page box's foot in a {:.0} px window",
                window.1
            );
            // 2. As wide as the box minus the column and one gap.
            let expected = frame.page_box.width() - GRAPH_GAP - graph::PANEL_WIDTH;
            assert!(
                (plate.width() - expected).abs() <= 2.0,
                "the plate is {:.0} px wide, not the {:.0} px left of the column",
                plate.width(),
                expected
            );
        }

        // 3. The height grows with the window, one pixel for the pixel it gained: no
        //    literal can do that, and 620 px at both sizes is what the bug did.
        let gained = tall.1 - short.1;
        let grew = tall_plate.height() - short_plate.height();
        assert!(
            (grew - gained).abs() <= 1.0,
            "{gained:.0} px more window bought {grew:.0} px more plate"
        );
    }

    #[test]
    fn the_graph_stacks_its_column_instead_of_painting_it_outside_the_window() {
        // The measured defect: at 700×520 the relations column's text ran to x = 859 in a
        // 700 px window — it was not clipped short, it was *outside*, because app always
        // reserved the column's 320 px after a plate that already had a 320 px floor. Now
        // the page stacks the two when they cannot sit side by side, and both stay inside
        // the page box at every size in the audit.
        for (width, height) in [
            (700.0_f32, 520.0_f32),
            (900.0, 620.0),
            (1280.0, 820.0),
            (2400.0, 1400.0),
        ] {
            let frame = painted_at(width, height, Page::Graph, None, Vec::new());
            let page = frame.page_box;
            let plate = frame.plate;
            let needs = Atlas::MIN_PLATE_WIDTH + GRAPH_GAP + graph::PANEL_WIDTH;
            let stacked = page.width() < needs;

            // 1. Both parts inside the page box.
            for (what, rect) in [("plate", plate)] {
                assert!(
                    rect.left() >= page.left() - 1.0
                        && rect.right() <= page.right() + 1.0
                        && rect.top() >= page.top() - 1.0
                        && rect.bottom() <= page.bottom() + 1.0,
                    "{what} {rect:?} is outside the page box {page:?} at {width:.0}x{height:.0}"
                );
            }
            // 2. Never narrower than its floor, as long as the page box itself is wider.
            let floor = Atlas::MIN_PLATE_WIDTH.min(page.width());
            assert!(
                plate.width() >= floor - 1.0,
                "the plate is {:.0} px wide at {width:.0}x{height:.0}, below its {floor:.0} px floor",
                plate.width()
            );
            // 3. The layout matches the box: stacked means the plate spans the width and the
            //    column begins below it; side by side means the column keeps its width at
            //    the right edge.
            if stacked {
                assert!(
                    (plate.width() - page.width()).abs() <= 1.0,
                    "a stacked plate is {:.0} px wide in a {:.0} px page box",
                    plate.width(),
                    page.width()
                );
                assert!(
                    plate.bottom() < page.bottom() - 1.0,
                    "a stacked plate leaves no room for the column below it"
                );
            } else {
                assert!(
                    (page.right() - plate.right() - graph::PANEL_WIDTH - GRAPH_GAP).abs() <= 2.0,
                    "the plate's right edge {} does not leave the column's {} px at the page's edge {}",
                    plate.right(),
                    graph::PANEL_WIDTH,
                    page.right()
                );
            }
            // 4. Nothing the page paints crosses its own right or left edge — the column's
            //    text used to end 178 px past the window.
            let runs = frame.on_the_page();
            assert!(!runs.is_empty(), "the page painted nothing at all");
            for run in &runs {
                assert!(
                    run.right() <= page.right() + 1.0 && run.left() >= page.left() - 1.0,
                    "a run {run:?} escapes the page box {page:?} at {width:.0}x{height:.0}"
                );
            }
            // 5. And the relations column is actually painted, on the far side of the plate:
            //    hiding it would pass the assertions above without being a fix.
            let column: Vec<egui::Rect> = runs
                .iter()
                .copied()
                .filter(|run| {
                    if stacked {
                        run.top() >= plate.bottom()
                    } else {
                        run.left() >= plate.right()
                    }
                })
                .collect();
            assert!(
                !column.is_empty(),
                "the relations column painted nothing at {width:.0}x{height:.0}"
            );
            // Measured, for the report and for the eye: where the column's content sits.
            let column_box = column.iter().fold(egui::Rect::NOTHING, |box_, run| box_.union(*run));
            println!(
                "{width:.0}x{height:.0}: page box {:.0}x{:.0}, plate {:.0}x{:.0} at ({:.0},{:.0}), \
                 column content {:.0}x{:.0} at ({:.0},{:.0}) — {} (needs {needs:.0} px to sit beside)",
                page.width(),
                page.height(),
                plate.width(),
                plate.height(),
                plate.left(),
                plate.top(),
                column_box.width(),
                column_box.height(),
                column_box.left(),
                column_box.top(),
                if stacked { "stacked" } else { "side by side" },
            );
        }
    }

    #[test]
    fn the_rail_folds_and_gives_its_width_to_the_page() {
        // The rail was a fixed 188 px: 26.9 % of a 700 px window (measured in the audit). It
        // folds to its marks below `RAIL_FOLDS_BELOW`, unfolds above it, and the shortcut
        // overrides both ways.
        let narrow = painted_at(700.0, 520.0, Page::Graph, None, Vec::new());
        let narrow_unfolded = painted_at(700.0, 520.0, Page::Graph, Some(false), Vec::new());
        let wide = painted_at(1280.0, 820.0, Page::Graph, None, Vec::new());
        let wide_folded = painted_at(1280.0, 820.0, Page::Graph, Some(true), Vec::new());
        let by_key = painted_at(
            1280.0,
            820.0,
            Page::Graph,
            None,
            vec![command_key(egui::Key::B)],
        );
        println!(
            "page box: 700 wide {} px (unfolded {}), 1280 wide {} px (folded {}), by key {} px",
            narrow.page_box.width(),
            narrow_unfolded.page_box.width(),
            wide.page_box.width(),
            wide_folded.page_box.width(),
            by_key.page_box.width()
        );

        // 1. Automatic: the narrow window folds (its page box is the window minus the folded
        //    rail and the page's two 20 px margins), the wide one does not.
        let folded_box = 700.0 - Atlas::RAIL_FOLDED - 40.0;
        let open_box = 1280.0 - Atlas::RAIL_WIDTH - 40.0;
        assert!(
            (narrow.page_box.width() - folded_box).abs() <= 2.0,
            "the 700 px window's page box is {:.0} px, not the {folded_box:.0} px a folded rail leaves",
            narrow.page_box.width()
        );
        assert!(
            (wide.page_box.width() - open_box).abs() <= 2.0,
            "the 1280 px window's page box is {:.0} px, not the {open_box:.0} px the full rail leaves",
            wide.page_box.width()
        );

        // 2. The page box gains exactly what the rail gives up.
        let gain = narrow.page_box.width() - narrow_unfolded.page_box.width();
        let expected = Atlas::RAIL_WIDTH - Atlas::RAIL_FOLDED;
        assert!(
            (gain - expected).abs() <= 1.0,
            "folding the rail gave the page {gain:.0} px, not {expected:.0}"
        );

        // 3. The shortcut does the same on a wide window, and matches the folded layout.
        assert!(
            (by_key.page_box.width() - wide_folded.page_box.width()).abs() <= 2.0,
            "ctrl/cmd+B gave a page box of {:.0} px, not the {:.0} px of a folded rail",
            by_key.page_box.width(),
            wide_folded.page_box.width()
        );
        assert!(
            by_key.page_box.width() > wide.page_box.width() + 1.0,
            "ctrl/cmd+B did not fold the rail"
        );

        // 4. And at 900 px, where the audit measured the canvas at exactly half the page box,
        //    the gain reaches the graph: the plate is wider by the width the rail gave up.
        let at_900 = painted_at(900.0, 620.0, Page::Graph, None, Vec::new());
        let at_900_open = painted_at(900.0, 620.0, Page::Graph, Some(false), Vec::new());
        let plate_gain = at_900.plate.width() - at_900_open.plate.width();
        println!(
            "900x620: page box {:.0} -> {:.0} px, plate {:.0} -> {:.0} px when the rail folds",
            at_900_open.page_box.width(),
            at_900.page_box.width(),
            at_900_open.plate.width(),
            at_900.plate.width()
        );
        assert!(
            (plate_gain - expected).abs() <= 1.0,
            "the rail's folding gave the plate {plate_gain:.0} px, not {expected:.0}"
        );

        // 5. And the folded rail's own contents fit the folded rail: the audit measured the
        //    full rail's last label 5 px from the window's edge, so a folded rail that spilled
        //    would be the same defect at a smaller width. Measured from the shapes it paints.
        for frame in [&narrow, &wide_folded] {
            let in_rail = frame.in_the_rail();
            assert!(!in_rail.is_empty(), "the folded rail painted nothing");
            let widest = in_rail
                .iter()
                .map(|run| run.right())
                .fold(f32::MIN, f32::max);
            println!(
                "folded rail: {:.0} px wide, its content ends at {:.0} px",
                frame.rail.width(),
                widest
            );
            assert!(
                widest <= frame.rail.right() + 1.0,
                "the folded rail's content runs to {widest:.0} px, past its own edge {:.0}",
                frame.rail.right()
            );
        }

        // 6. Folded, it shows the marks and not the words — and every page still has its
        //    mark, so nothing silently disappears from the rail.
        let folded_words = wide_folded.rail_texts();
        let full_words = wide.rail_texts();
        for page in PAGES {
            assert!(
                folded_words.contains(&page.mark()),
                "the folded rail does not show {page:?}'s mark; it shows {folded_words:?}"
            );
            assert!(
                !folded_words.contains(&page.label()),
                "the folded rail still spells out {page:?}"
            );
            assert!(
                full_words.contains(&page.label()),
                "the full rail no longer shows {page:?}'s name; it shows {full_words:?}"
            );
        }
    }

    #[test]
    fn every_door_in_the_rail_answers_and_every_count_opens_the_page_it_counts() {
        // A blind-click audit found the rail's totals inert: `7`, `5 entities`,
        // `0 connections`, `3 topics`, `2 monitors` were painted as text and answered
        // nothing, while the window chips and Refresh did work. This clicks every control
        // the rail paints, in a headless frame of the real window, and asks what changed.
        // 1280 px is wide enough for the full rail, which is the rail that shows counts.
        let (width, height) = (1280.0_f32, 820.0_f32);
        // The labels, built the way the rail builds them, from an empty archive (the
        // fixture has no file), and the page each count must open.
        let empty = Counts::default();
        let counts: Vec<(String, Page)> = vec![
            (nf(empty.relevant), Page::Articles),
            (format!("{} entities", nf(empty.entities)), Page::Graph),
            (format!("{} connections", nf(empty.links)), Page::Graph),
            (format!("{} topics", nf(empty.topics)), Page::Topics),
            (format!("{} monitors", nf(empty.monitors)), Page::Monitors),
        ];

        // 1. Every page row opens its page — from a *different* page, so "already there"
        //    cannot pass for a working door.
        for page in PAGES {
            let from = if page == Page::Chat {
                Page::Overview
            } else {
                Page::Chat
            };
            let (now, hours, ..) = click_rail(width, height, (from, 72), page.label());
            assert_eq!(
                now,
                page,
                "clicking `{}` in the rail did not open {page:?}",
                page.label()
            );
            assert_eq!(hours, 72, "clicking `{}` changed the window", page.label());
            println!("rail `{}` → {page:?}", page.label());
        }

        // 2. Each total is a door to the page it counts, and says so on hover.
        for (label, page) in &counts {
            let (now, hours, ..) = click_rail(width, height, (Page::Chat, 72), label);
            assert_eq!(
                now, *page,
                "clicking the count `{label}` opened {now:?}, not {page:?}"
            );
            assert_eq!(hours, 72, "clicking the count `{label}` changed the window");
            let hover = Atlas::door_hover(*page);
            assert!(
                hover.contains(page.label()),
                "the hover for a `{label}` door is `{hover}`, which does not name {page:?}"
            );
            println!("rail count `{label}` → {page:?} (hover \"{hover}\")");
        }

        // 3. The window chips still set the window, and only the window.
        for window in [24_i64, 72, 168] {
            let from = if window == 72 { 24 } else { 72 };
            let (now, hours, ..) =
                click_rail(width, height, (Page::Chat, from), Atlas::hours_label(window));
            assert_eq!(
                hours,
                window,
                "the `{}` chip did not set the window",
                Atlas::hours_label(window)
            );
            assert_eq!(
                now,
                Page::Chat,
                "the `{}` chip also changed the page",
                Atlas::hours_label(window)
            );
            println!("rail `{}` → {hours} h", Atlas::hours_label(window));
        }

        // 4. Refresh still works, and still only re-reads.
        let (now, hours, before, after) = click_rail(width, height, (Page::Chat, 72), "Refresh");
        assert_eq!(
            (now, hours),
            (Page::Chat, 72),
            "Refresh changed the page or the window"
        );
        assert!(
            after > before,
            "Refresh did not read the archive (reads {before} then {after})"
        );
        println!("rail `Refresh` → reads {before} then {after}, page {now:?}");

        // 5. The lockup is not a control: the mark and the name stay text, untouched.
        for inert in ["ATLAS", "local news graph"] {
            let (now, hours, before, after) = click_rail(width, height, (Page::Chat, 24), inert);
            assert_eq!(
                (now, hours),
                (Page::Chat, 24),
                "`{inert}` answered a click; the lockup is not a door"
            );
            assert_eq!(
                after, before,
                "`{inert}` triggered a read; the lockup is not a door"
            );
            println!("rail `{inert}` → nothing (lockup)");
        }

        // 6. Folded, the counts are hidden on purpose — a 64 px column has no room for
        //    "5 connections", and the folded rail reaches the same pages through its marks.
        let folded = painted_at(700.0, 520.0, Page::Graph, None, Vec::new());
        let folded_words = folded.rail_texts();
        for (label, _) in &counts {
            assert!(
                !folded_words.contains(&label.as_str()),
                "the folded rail paints the count `{label}`; it is meant to be hidden"
            );
        }
        println!("folded rail shows {:?}", folded_words);

        // 7. And the rail still holds its own content at the narrowest window that shows
        //    the full rail: a door is a control, and a control is a 24 px target, so the
        //    counts block is taller than the labels it replaced. If that ever stops fitting,
        //    the counts would be clipped rather than hidden — this is the check.
        let narrowest = Atlas::RAIL_FOLDS_BELOW;
        let full = painted_at(narrowest, 520.0, Page::Graph, Some(false), Vec::new());
        let bottom = full
            .in_the_rail()
            .iter()
            .map(|run| run.bottom())
            .fold(f32::MIN, f32::max);
        println!(
            "the full rail at {narrowest:.0}x520: content ends at {bottom:.0} px, rail box ends at {:.0}",
            full.rail.bottom()
        );
        assert!(
            bottom <= full.rail.bottom() - 1.0,
            "the full rail's content runs to {bottom:.0} px in a 520 px window (rail ends {:.0})",
            full.rail.bottom()
        );
    }

    #[test]
    fn both_status_labels_stay_inside_the_row_at_every_window_size() {
        // The measured defect: with `with_layout(right_to_left)` nested in a `horizontal`,
        // the right label's rect started 13 px from the window edge and was 146 px wide, so
        // 133 px of it sat outside the clip at every size on every page — the clip hid it,
        // and nothing in the code could notice. The rule the test now pins is the general
        // one: *no* text the status row paints leaves the clip it was given, and none leaves
        // the window.
        for (width, height) in [(700.0_f32, 520.0_f32), (1440.0, 900.0), (2400.0, 1400.0)] {
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            for page in [Page::Overview, Page::Chat] {
                let frame = painted_at(width, height, page, None, Vec::new());
                let in_the_band: Vec<(egui::Rect, egui::Rect, &str)> = frame
                    .clipped_texts()
                    .into_iter()
                    // The status row is the strip below the central panel's page box: a run
                    // belongs to it when its *clip* is that strip — the last band of the
                    // window, from just under the page box to the bottom edge. The rect alone
                    // is not enough: a page's own last line can sit below the page box's
                    // bottom margin and would be counted as the row.
                    .filter(|(_, clip, _)| {
                        clip.top() >= frame.page_box.bottom() - 4.0
                            && clip.bottom() >= height - 1.0
                    })
                    .collect();
                let right = if page == Page::Chat {
                    CHAT_NEEDS_MODEL
                } else {
                    "native window · local archive"
                };
                // Not vacuous: the row paints exactly two labels — the left one (the counts,
                // or the sentence that says what went wrong instead) and the right one — and
                // both are found by their own text.
                assert_eq!(
                    in_the_band.len(),
                    2,
                    "{page:?} at {width:.0}×{height:.0}: the status row paints {} runs, not \
                     two: {:?}",
                    in_the_band.len(),
                    in_the_band
                );
                let left_run = in_the_band
                    .iter()
                    .find(|(_, _, text)| *text != right)
                    .map(|(rect, _, text)| (*rect, *text))
                    .expect("a left label");
                assert!(
                    left_run.0.width() > 0.0 && !left_run.1.is_empty(),
                    "{page:?} at {width:.0}×{height:.0}: the left label is empty"
                );
                assert!(
                    in_the_band.iter().any(|(_, _, text)| *text == right),
                    "{page:?} at {width:.0}×{height:.0}: the row paints no `{right}`: {:?}",
                    in_the_band
                );
                for (rect, clip, text) in &in_the_band {
                    assert!(
                        clip.contains_rect(*rect),
                        "{page:?} at {width:.0}×{height:.0}: `{text}` is painted at {rect:?} \
                         but its clip is {clip:?}"
                    );
                    assert!(
                        window.contains_rect(*rect),
                        "{page:?} at {width:.0}×{height:.0}: `{text}` is painted at {rect:?}, \
                         outside the window {window:?}"
                    );
                }
                // And the right label ends at the row's right edge, not at an arbitrary x:
                // a label placed by measured arithmetic lands within one space unit of it.
                let right_run = in_the_band
                    .iter()
                    .find(|(_, _, text)| *text == right)
                    .map(|(rect, _, _)| *rect)
                    .expect("the right label was just asserted to be painted");
                assert!(
                    right_run.right() <= width - 12.0 && right_run.right() >= width - 12.0 - theme::space::UNIT,
                    "the right label ends at {:.0} px in a {width:.0} px window",
                    right_run.right()
                );
            }
        }
    }

    #[test]
    fn no_page_types_a_height_or_wraps_a_page_that_scrolls_itself() {
        // Structural, and cheap: the two mistakes this page made are visible in the source.
        // A literal height (`620.0`) is a size that ignores the window; a second scroll area
        // around a page that owns one (`articles.rs`, `chat.rs`, `topics.rs`,
        // `monitors.rs` all show their own `ScrollArea`) makes the wheel move two regions.
        let code = code_only(shipped_input());
        assert!(
            code.contains("fn graph_page("),
            "the scan lost the Graph page — it is checking nothing"
        );
        assert!(
            !code.contains("620.0"),
            "app.rs still allocates a 620 px canvas: the plate's height is `available_height`"
        );
        // Not one scroll area: every page owns the scrolling it needs (Overview's story
        // list, Articles' rows, Chat's transcript, the Graph's plate), and a wrapper here
        // would hand a page an unbounded height. The Overview used to be wrapped, and the
        // price was measured: its card ran 286 px past the bottom of a 900 px window
        // because `ui.available_height()` was infinite.
        assert_eq!(
            code.matches("ScrollArea").count(),
            0,
            "app.rs shows {} scroll areas; the pages own their own",
            code.matches("ScrollArea").count()
        );
        assert!(
            code.contains("Page::Overview => self.overview_page(ui),"),
            "the Overview is wrapped again: it must draw in the box it is given"
        );
        // Every `allocate_ui` gives its child a *measured* size: the second component of
        // its `vec2` must be a name, never a number.
        let mut from = 0;
        while let Some(found) = code[from..].find("allocate_ui(") {
            let open = from + found;
            let after = open + "allocate_ui(".len();
            let window: String = code[after..].chars().take(120).collect();
            if let Some(vec_at) = window.find("vec2(") {
                let args = &window[vec_at + "vec2(".len()..];
                let args = &args[..args.find(')').unwrap_or(args.len())];
                if let Some((_, height_arg)) = args.split_once(',') {
                    let height_arg = height_arg.trim();
                    assert!(
                        height_arg.parse::<f32>().is_err(),
                        "app.rs calls `allocate_ui` with the literal height `{height_arg}`: \
                         measure it from `ui.available_height()`"
                    );
                }
            }
            from = after;
        }
    }

    /// The shipped half of this file: everything above this test module.
    ///
    /// The same technique the drawing test above uses — a window cannot be opened here, so
    /// a claim about what a view *asks for* is made about the source it asks it in.
    fn shipped_input() -> &'static str {
        let source: &'static str = include_str!("app.rs");
        source.split("#[cfg(test)]").next().unwrap_or(source)
    }

    /// The code in `source`, with comments removed.
    ///
    /// The scan below looks for calls, and a call is code. Prose that *names* a call — the
    /// comment on the lockup explains what `.strong()` resolves to — is not a defect, and
    /// a scanner that counted it would either report a comment or force the comments to
    /// stop naming the thing they explain. String literals are kept (a URL's `//` is not a
    /// comment), and the two comment forms are both handled so neither can hide a call.
    fn code_only(source: &str) -> String {
        #[derive(Clone, Copy, PartialEq)]
        enum State {
            Code,
            Str,
            Line,
            Block,
        }
        let mut out = String::with_capacity(source.len());
        let mut state = State::Code;
        let mut escaped = false;
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            match state {
                State::Str => {
                    out.push(c);
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        state = State::Code;
                    }
                }
                State::Line => {
                    if c == '\n' {
                        out.push(c);
                        state = State::Code;
                    }
                }
                State::Block => {
                    if c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        state = State::Code;
                    }
                }
                State::Code => match c {
                    '"' => {
                        out.push(c);
                        state = State::Str;
                    }
                    '/' if chars.peek() == Some(&'/') => {
                        chars.next();
                        state = State::Line;
                    }
                    '/' if chars.peek() == Some(&'*') => {
                        chars.next();
                        state = State::Block;
                    }
                    _ => out.push(c),
                },
            }
        }
        out
    }

    #[test]
    fn no_label_in_the_rail_claims_a_weight_or_a_size_of_its_own() {
        // Two ways to write emphasis the theme cannot draw, both of which read as if they
        // did something:
        //
        // * `.strong()` — egui resolves it to `widgets.active.text_color()`
        //   (`widget_text.rs:504`), which this theme sets to the same INK body text
        //   already has, so it draws *identically* to no call at all. The rail's name and
        //   both of its selected states wore it; nothing on screen changed when they did.
        // * `.size(15.0)` — a second copy of the ladder. It does not move when
        //   `theme::size` moves, and no test compares the two numbers, so a change to the
        //   scale would silently skip this string.
        //
        // A role answers both at once: a size on the ladder, a face, and an ink named
        // where it is decided.
        let shipped = shipped_input();
        let code = code_only(shipped);
        assert!(
            code.contains("fn rail("),
            "the scan lost the rail — it is checking nothing"
        );
        assert!(
            !code.contains(".strong()"),
            "app.rs asks for `.strong()`, which egui draws in the same ink as body text: \
             use a role (`theme::card_title`, `theme::body`, `theme::section_label`)"
        );
        assert!(
            !code.contains(".weak()"),
            "app.rs asks for `.weak()`, which is a colour (`weak_text_color` = DIM) and not \
             a lighter weight: name the ink (`theme::dim`, `.color(theme::DIM)`) so the \
             source shows what is drawn"
        );
        // Every `.size(...)` argument must be a named step, not a number.
        let mut from = 0;
        while let Some(found) = code[from..].find(".size(") {
            let after = from + found + ".size(".len();
            let argument: String = code[after..].trim_start().chars().take(12).collect();
            assert!(
                !argument.starts_with(|c: char| c.is_ascii_digit() || c == '.'),
                "app.rs calls `.size({argument}…)` with a literal: name a step of the ladder \
                 (`theme::size::*`) or a role, so the size moves with the scale"
            );
            from = after;
        }
    }

    #[test]
    fn the_lockup_and_the_rows_keep_the_pixels_they_had() {
        // "Honest emphasis, not a redesign": the retired literals were 15 px of
        // proportional text in INK for the name (`.size(15.0)`, ink from
        // `override_text_color`), and the default body style — 13 px, proportional — in
        // INK or DIM for the rows. This measures that the roles still answer those
        // numbers, because a source scan cannot see a pixel.
        let style = theme::styled(egui::Style::default());
        let default_body = style.text_styles[&egui::TextStyle::Body].clone();

        // the name: 15 px, the text face, full ink
        assert_eq!(theme::size::TITLE, 15.0, "the lockup used to be 15 px");
        assert_eq!(theme::role::CARD_TITLE.size, theme::size::TITLE);
        assert_eq!(theme::role::CARD_TITLE.colour, theme::INK);
        assert_eq!(
            theme::role::CARD_TITLE.weight.family(),
            default_body.family,
            "the name changed face"
        );
        assert_eq!(
            theme::card_title("ATLAS").color_override(),
            Some(theme::INK),
            "the name changed ink"
        );

        // the rows: the size and face of an un-styled body label, in INK or DIM
        assert_eq!(
            theme::role::BODY.size, default_body.size,
            "the rows are no longer the size an un-styled label had"
        );
        assert_eq!(theme::role::BODY.weight.family(), default_body.family);
        assert_eq!(
            theme::body("Overview").color_override(),
            Some(theme::INK),
            "a selected row changed ink"
        );
        assert_eq!(
            theme::body("Overview").color(theme::DIM).color_override(),
            Some(theme::DIM),
            "an unselected row changed ink"
        );

        // and the rail asks for exactly those, so the measurement above is about the
        // pixels the window actually draws. The rows pick their label first — the page's
        // name in a full rail, its initial when folded — and hand it to the same role.
        let shipped = shipped_input();
        for asked in [
            "theme::card_title(\"ATLAS\")",
            "theme::body(label)",
            "page.mark()",
            "page.label()",
        ] {
            assert!(
                shipped.contains(asked),
                "the rail no longer calls `{asked}`, so the role above is not what it draws"
            );
        }
    }

    // ------------------------------------------- the rollup doors, against the real archive

    /// The checkout's archive, or `None` for a bare clone: the door tests measure a chip
    /// against the rows the chip counted, and a clone has none. Read-only, always —
    /// [`Archive::open`] passes `SQLITE_OPEN_READ_ONLY` (`src/data.rs`).
    fn live_archive() -> Option<(std::path::PathBuf, Archive)> {
        let path = crate::assets::archive();
        if !path.is_file() {
            eprintln!(
                "no archive at {} — nothing to count; the doors are not measured",
                path.display()
            );
            return None;
        }
        match Archive::open(&path) {
            Ok(archive) => Some((path, archive)),
            Err(sentence) => {
                eprintln!("{sentence} — nothing to count");
                None
            }
        }
    }

    /// The window a door test drives: the shipped archive, and a scratch queue so a click
    /// can never write the live `data/requests.db`.
    fn door_window(path: std::path::PathBuf) -> Atlas {
        Atlas::with_archive(path).with_queue(
            std::env::temp_dir().join("atlas-desktop-door-queue.db"),
        )
    }

    #[test]
    fn a_country_chip_opens_the_list_it_counts() {
        // The user's sentence: "I pick a filter and simply nothing comes". The chip's number
        // comes from `Archive::countries` (grouped by `iso`); the list its click opened came
        // from `Archive::place_stories` (matched against the place *label*), and a country
        // whose stories all name a city has no row under its own label. Measured on the
        // shipped archive before the fix: `USA` said 105 stories and the list held 0.
        //
        // This test is the whole path a reader takes: the chip the page *painted*, clicked
        // at the pixel it was painted at, through the window, into the list on Articles.
        let Some((path, archive)) = live_archive() else {
            return;
        };
        let countries = archive.countries(72, COUNTRY_ROWS);
        let Some((iso, count)) = countries.first().cloned() else {
            eprintln!("no country in the window — nothing to click");
            return;
        };
        let mut atlas = door_window(path);
        atlas.page = Page::Overview;
        let context = egui::Context::default();
        let label = format!("{iso} {}", nf(count));
        let first = draw_frame(&context, &mut atlas, 1440.0, 900.0, Vec::new());
        let at = first.page_run(&label).unwrap_or_else(|| {
            panic!(
                "the page paints no chip labelled {label:?}; it paints {:?}",
                first.page_texts()
            )
        });
        for pressed in [true, false] {
            draw_frame(
                &context,
                &mut atlas,
                1440.0,
                900.0,
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
        // The frame that lands the list the click asked for.
        let after = draw_frame(&context, &mut atlas, 1440.0, 900.0, Vec::new());

        assert_eq!(
            atlas.page,
            Page::Articles,
            "a click on the {iso} chip ({count} stories) left the window on {:?}",
            atlas.page
        );
        let door = atlas
            .place
            .clone()
            .unwrap_or_else(|| panic!("the {iso} chip ({count} stories) opened no door"));
        assert_eq!(
            door.kind,
            Kind::Country,
            "the {iso} chip is a row of `Archive::countries` but its door says {:?}",
            door.kind
        );
        assert_eq!(door.word, iso);
        let held = atlas.snapshot.articles.len() as i64;
        assert!(
            held > 0,
            "the {iso} chip says {count} stories and the list it opens holds none"
        );
        assert_eq!(
            held,
            count.min(LIST_ROWS),
            "the {iso} chip says {count} stories and its list holds {held}"
        );
        // …and the strip above that list prints the number the list *holds*, so the page
        // cannot say one number over a list of another length.
        let said = if count > held {
            format!("{} of {} from {iso}", nf(held), nf(count))
        } else {
            format!("{} from {iso}", nf(held))
        };
        assert!(
            after.page_texts().contains(&said.as_str()),
            "the strip above the {iso} list does not say {said:?}; the page paints {:?}",
            after.page_texts()
        );
    }

    #[test]
    fn no_chip_can_claim_a_story_it_cannot_open() {
        // Every chip on the Overview page, not just the first: the promise is per chip, and
        // it is kept by reading through the one hand-off that knows the kind
        // ([`stories_for`]). Each list must hold exactly `count.min(LIST_ROWS)` rows and every
        // row must resolve to a story that opens.
        let Some((_, archive)) = live_archive() else {
            return;
        };
        let countries = archive.countries(72, COUNTRY_ROWS);
        assert!(
            !countries.is_empty(),
            "the country rollup is empty in this window"
        );
        let mut claimed = 0i64;
        let mut by_label = 0i64;
        for (iso, count) in &countries {
            let door = Door::country(iso, *count);
            let list = stories_for(&archive, &door, 72, LIST_ROWS);
            let as_place = archive.place_stories(iso, 72, LIST_ROWS).len();
            claimed += list.len() as i64;
            by_label += as_place as i64;
            assert_eq!(
                list.len() as i64,
                (*count).min(LIST_ROWS),
                "the {iso} chip says {count} stories and its door opens {} (labeled `{iso}`: \
                 {as_place})",
                list.len()
            );
            assert!(
                !list.is_empty(),
                "the {iso} chip ({count} stories) opens nothing"
            );
            let ids: Vec<i64> = list.iter().map(|article| article.id).collect();
            let held: std::collections::BTreeSet<i64> = archive
                .articles_by_ids(&ids)
                .into_iter()
                .map(|article| article.id)
                .collect();
            assert!(
                ids.iter().all(|id| held.contains(id)),
                "the {iso} list names a story that does not open"
            );
        }
        // The measurement the fix is about, on this archive: reading the chips by their
        // label would have opened far fewer stories than they count.
        println!(
            "doors: {} chips claim {claimed} stories; the same words read as place labels \
             would open {by_label}",
            countries.len()
        );
        assert!(
            by_label < claimed,
            "the two reads agree on this archive, so nothing here proves the kind decides \
             ({claimed} vs {by_label})"
        );
    }

    #[test]
    fn a_place_door_opens_the_stories_its_pin_counted() {
        // The map's dots are *places* — the grouping `Archive::pins` uses — so a place door
        // must open that label's stories, including the dot's own destination story. This is
        // the read the merged-dot door needs the day `views::map::Cluster` can name its
        // members; it is proved here so that day is a one-line change in `map_card`.
        let Some((_, archive)) = live_archive() else {
            return;
        };
        let pins = archive.pins(72, LIST_ROWS);
        let pin = pins
            .iter()
            .find(|pin| pin.count > 0 && !pin.place.trim().is_empty());
        let Some(pin) = pin else {
            eprintln!("no located story in the window — no place to open");
            return;
        };
        let door = Door::place(&pin.place, pin.count);
        let list = stories_for(&archive, &door, 72, LIST_ROWS);
        assert_eq!(
            list.len() as i64,
            pin.count.min(LIST_ROWS),
            "the dot at {} says {} stories and the place door opens {}",
            pin.place,
            pin.count,
            list.len()
        );
        assert!(
            list.iter().any(|article| article.id == pin.article),
            "the place {} opens a list without the story its dot stands for ({})",
            pin.place,
            pin.article
        );

        // And the kind is not decoration: on a word that is both a country rollup and a
        // place label an archive holds, the two doors must be two different lists — that is
        // exactly what `Door`'s kind carries and what the old bare `String` could not.
        let mut differed = None;
        for pin in pins.iter().take(50) {
            if pin.place.trim().is_empty() {
                continue;
            }
            let by_place = stories_for(&archive, &Door::place(&pin.place, pin.count), 72, LIST_ROWS);
            let by_country = stories_for(
                &archive,
                &Door::country(&pin.place, pin.count),
                72,
                LIST_ROWS,
            );
            let place_ids: Vec<i64> = by_place.iter().map(|article| article.id).collect();
            let country_ids: Vec<i64> = by_country.iter().map(|article| article.id).collect();
            if place_ids != country_ids {
                differed = Some((pin.place.clone(), place_ids.len(), country_ids.len()));
                break;
            }
        }
        let (word, as_place, as_country) = differed.unwrap_or_else(|| {
            panic!(
                "no place label in the window reads differently as a country, so nothing here \
                 proves the door's kind decides which read runs"
            )
        });
        println!(
            "doors: `{word}` opens {as_place} stories as a place and {as_country} as a country"
        );
    }

    #[test]
    fn the_same_word_of_two_kinds_is_two_reads() {
        // A door is part of the demand, kind included: `USA` as a country rollup and `USA` as
        // a place label are different rows, so switching between them is a new read.
        let country = Demand::of(72, "", Some(Door::country("USA", 105)), None, None);
        let place = Demand::of(72, "", Some(Door::place("USA", 105)), None, None);
        assert!(
            needs_read(Some(&country), &place, false),
            "the country `USA` and the place `USA` are one demand: one of the two would draw \
             the other's stories"
        );
        assert!(!needs_read(Some(&country), &country.clone(), false));

        // A door with no word is no door: a chip drawn for an empty `iso` cannot become a
        // list of nothing that claims a number.
        assert_eq!(crate::views::overview::Choice::default().door(), None);
        assert_eq!(
            Demand::of(72, "", Some(Door::country("   ", 3)), None, None),
            Demand::of(72, "", None, None, None),
            "a blank door is not a read"
        );

        let mut choice = crate::views::overview::Choice::default();
        choice.at_country("USA", 105);
        assert_eq!(choice.door(), Some(Door::country("USA", 105)));
        assert!(!choice.nothing(), "a chip the window acts on is not nothing");
        choice.at_place("USA", 105);
        assert_eq!(choice.door(), Some(Door::place("USA", 105)));
    }

    #[test]
    fn a_door_word_reaches_the_archive_only_through_stories_for() {
        // The structural half: there is exactly one place where a door becomes a read, so a
        // chip cannot count one grouping and open another — the defect was two reads of the
        // same word, twenty lines apart, and no test could see the difference.
        const SOURCE: &str = include_str!("app.rs");
        let shell = SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("the shell before its tests");
        for call in [".country_stories(", ".place_stories("] {
            assert_eq!(
                shell.matches(call).count(),
                1,
                "`{call}` is called from more than one place in the shell: the door's kind must \
                 be the only thing that decides which read runs"
            );
        }
        let start = shell.find("pub fn stories_for(").expect("the door's read");
        let end = start + shell[start..].find("\n}\n").expect("the end of it");
        let body = &shell[start..end];
        for call in [".country_stories(", ".place_stories("] {
            assert!(
                body.contains(call),
                "`{call}` is not inside `stories_for`, so a chip can still open a list the \
                 chip did not count"
            );
        }
        // The window stores a door the page handed over, and clears it when the strip is
        // pressed. Nothing else may invent one.
        let mut handed = 0;
        for line in shell.lines().map(str::trim) {
            if !line.starts_with("self.place =") {
                continue;
            }
            handed += 1;
            assert!(
                line == "self.place = Some(door);"
                    || line == "self.place = None;"
                    // Restoring is not inventing: `go_back` puts back the door the page
                    // handed over before the story was opened (`Origin::place`), so the value
                    // still came from `Choice::door()` and nowhere else.
                    || line == "self.place = origin.place;",
                "the shell builds a door by hand (`{line}`): a door is only ever the page's \
                 `Choice::door()` or nothing"
            );
        }
        assert_eq!(
            handed, 4,
            "the door is handed over somewhere new: the page's choice, the strip's clear, the \
             clear a story- click makes, and `go_back`'s restore"
        );
    }
    /// The workflow the reader reported broken: a country chip in force on the Overview, a
    /// story opened from the Graph, and then no way to understand or leave the page they
    /// landed on.
    ///
    /// Measured with the shipped build: the German chip stayed in force, so the Articles page
    /// showed four hundred German stories with one Russian story appended to the bottom, and
    /// nothing returned to the Graph. This pins the three promises that fix it: the story is
    /// opened on its own, its row is asked for on the same frame, and `go_back` restores the
    /// page *and* the chip.
    #[test]
    fn a_story_opened_elsewhere_is_read_on_its_own_and_comes_back_where_it_came_from() {
        // A shell with no archive is enough: this is a test of the shell's state machine, not
        // of a query. `open_article` writes no file and reads nothing.
        let mut atlas = Atlas::with_archive(PathBuf::from("/nonexistent/atlas.db"));
        atlas.page = Page::Graph;
        atlas.place = Some(Door::country("DEU", 400));
        atlas.articles_view.needle = "chips".to_string();
        atlas.topic_open = Some(3);
        atlas.articles_view.selected = Some(11);

        atlas.open_article(4242);

        assert_eq!(atlas.page, Page::Articles, "the click opens the story");
        assert_eq!(
            atlas.articles_view.selected,
            Some(4242),
            "the story the reader clicked is the one open"
        );
        assert_eq!(
            atlas.place, None,
            "the German chip is still narrowing the page under a Russian story"
        );
        assert_eq!(atlas.topic_open, None, "a topic row does not narrow a story either");
        assert!(atlas.articles_view.needle.is_empty(), "and neither does a search box");
        assert_eq!(
            atlas.reveal,
            Some(4242),
            "the row has to be brought into view on the frame that opens it"
        );
        let origin = atlas.origin.clone().expect("the page it came from is remembered");
        assert_eq!(origin.page, Page::Graph);
        assert_eq!(origin.place, Some(Door::country("DEU", 400)));

        atlas.go_back();

        assert_eq!(atlas.page, Page::Graph, "back returns to the page the story was found on");
        assert_eq!(
            atlas.place,
            Some(Door::country("DEU", 400)),
            "the chip the reader had is not thrown away by looking at one story"
        );
        assert_eq!(atlas.topic_open, Some(3));
        assert_eq!(atlas.articles_view.needle, "chips");
        assert_eq!(
            atlas.articles_view.selected, None,
            "the pane closes: the reader is back on the page they came from"
        );
        assert!(atlas.origin.is_none(), "back consumes the origin");
    }

    /// A story opened while already on the Articles page keeps the origin it arrived with: the
    /// list's own clicks are not a new place to return to.
    /// The engine view is a child **window**, not a widget: leaving the Reader has to close it.
    ///
    /// Measured in the shipped window: a source page was open, the reader pressed the map, and
    /// the page stayed on top of it — the plate could not be clicked, and nothing in the window
    /// could close the view. The cause is structural (an egui panel cannot clip a child web
    /// view), so the rule is too: the view lives exactly as long as the page does.
    #[test]
    fn leaving_the_reader_closes_the_engine_view_and_the_story_with_it() {
        let mut atlas = Atlas::with_archive(PathBuf::from("/nonexistent/atlas.db"));
        atlas.open_reader(crate::views::reader::Story {
            id: 3,
            title: "A story".to_string(),
            source: "Wire".to_string(),
            published: "2026-11-22T09:00:00Z".to_string(),
            url: "https://example.invalid/a".to_string(),
            text: "Words.".to_string(),
            full_text: true,
        });
        assert_eq!(atlas.page, Page::Reader);
        assert!(atlas.reader_story.is_some(), "the story was not kept");

        // The rail: any other page closes it.
        atlas.set_page(Page::Graph);
        assert!(
            atlas.reader_story.is_none(),
            "the source page was still open after the reader left it"
        );
        assert!(
            atlas.reader_page.is_none(),
            "the engine view outlived the page it was showing"
        );

        // Back to the Reader: an invitation, not a stale address with no engine behind it.
        atlas.set_page(Page::Reader);
        assert!(atlas.reader_story.is_none());
        assert!(atlas.reader_page.is_none());

        // The other three doors close it too: a story opened from the Reader, a chip, and the
        // way back (which lands on the page the reader came from).
        for close in 0..3 {
            atlas.open_reader(crate::views::reader::Story {
                id: 3,
                title: "A story".to_string(),
                source: "Wire".to_string(),
                published: "2026-11-22T09:00:00Z".to_string(),
                url: "https://example.invalid/a".to_string(),
                text: "Words.".to_string(),
                full_text: true,
            });
            assert!(atlas.reader_story.is_some());
            match close {
                0 => atlas.open_article(5),
                1 => atlas.narrow_to(crate::views::overview::Door::country("DEU", 2)),
                _ => atlas.go_back(),
            }
            assert!(
                atlas.reader_page.is_none() && atlas.reader_story.is_none(),
                "door {close} left the engine view alive"
            );
        }
    }

    #[test]
    fn opening_the_next_story_does_not_repoint_the_way_back() {
        let mut atlas = Atlas::with_archive(PathBuf::from("/nonexistent/atlas.db"));
        atlas.page = Page::Topics;
        atlas.open_article(7);
        let first = atlas.origin.clone().expect("an origin");
        atlas.open_article(8);
        assert_eq!(atlas.origin.as_ref(), Some(&first), "the way back still leads to Topics");
        assert_eq!(atlas.articles_view.selected, Some(8));
    }

    // -----------------------------------------------------------------------
    // the rail's window: measured, not read off a field
    // -----------------------------------------------------------------------

    /// A `data/atlas.db` this test owns: three stories, one in each window.
    ///
    /// 1 h, 30 h and 100 h old, so `24 h` is one story, `3 d` two and `7 d` three: a chip
    /// that does nothing is a list that does not move, at every chip.
    fn archive_with_three_windows(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-desktop-hours-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("atlas.db");
        let conn = rusqlite::Connection::open(&file).expect("a database");
        conn.execute_batch(
            "CREATE TABLE articles(
                 id INTEGER PRIMARY KEY, title TEXT, publisher TEXT, source_id INTEGER,
                 published TEXT, summary TEXT, content TEXT, iso TEXT, place TEXT,
                 url TEXT, full_text INTEGER, relevant INTEGER, dupe INTEGER,
                 lat REAL, lon REAL);
             CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE entities(
                 id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER, last_seen TEXT);
             CREATE TABLE edges(a INTEGER, b INTEGER, weight REAL, label TEXT);
             CREATE TABLE article_links(a INTEGER, b INTEGER);
             CREATE TABLE topics(
                 id INTEGER PRIMARY KEY, name TEXT, keywords TEXT, active INTEGER,
                 hits INTEGER, last_hit TEXT);
             CREATE TABLE monitors(
                 id INTEGER PRIMARY KEY, name TEXT, query TEXT, active INTEGER,
                 cadence_minutes INTEGER, last_run TEXT, hits INTEGER);
             CREATE TABLE alerts(id INTEGER PRIMARY KEY);
             CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT);",
        )
        .expect("the schema data.rs reads");
        conn.execute("INSERT INTO sources(id, name) VALUES(1, 'Wire Daily')", [])
            .expect("a source");
        for (id, hours, title) in [(1_i64, 1_i64, "An hour ago"), (2, 30, "A day ago"), (3, 100, "Four days ago")] {
            conn.execute(
                "INSERT INTO articles(id, title, publisher, source_id, published, summary, \
                 content, iso, place, url, full_text, relevant, dupe) \
                 VALUES(?1, ?2, '', 1, ?3, ?4, ?5, 'DEU', 'Berlin', ?6, 1, 1, 0)",
                rusqlite::params![
                    id,
                    title,
                    hours_ago_stamp(hours),
                    format!("{title} — a summary."),
                    format!("{title}. The body."),
                    format!("https://example.invalid/{id}")
                ],
            )
            .expect("a story");
        }
        file
    }

    /// `now - hours` as the archive's own stamp, the same algorithm `tests/smoke.rs` uses.
    fn hours_ago_stamp(hours: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|spent| spent.as_secs() as i64)
            .unwrap_or(0);
        let at = now - hours * 3_600;
        let (days, secs) = (at.div_euclid(86_400), at.rem_euclid(86_400));
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let year = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if month <= 2 { year + 1 } else { year };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year,
            month,
            day,
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }

    /// The status row's left label, as painted: the one line that counts stories and reads.
    fn status_line(painted: &Painted) -> String {
        painted
            .texts
            .iter()
            .map(|(_, _, text)| text.as_str())
            .find(|text| text.contains("read in "))
            .unwrap_or("")
            .to_string()
    }

    /// One frame of the real window, and what it asked the window system for.
    ///
    /// `Duration::ZERO` is "paint again now"; `Duration::MAX` is "sleep until an event" — the
    /// two numbers [`Context::request_repaint`] sets, so a test can tell a control that knows
    /// it changed from one that leaves the rail a frame behind, and can tell both from a
    /// frame that polls.
    fn frame_asking(
        ctx: &egui::Context,
        atlas: &mut Atlas,
        events: Vec<egui::Event>,
    ) -> (Painted, std::time::Duration) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(WINDOW_W, WINDOW_H),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| atlas.draw(ui));
        let shapes = std::mem::take(&mut output.shapes);
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|viewport| viewport.repaint_delay)
            .unwrap_or_default();
        output.drop_without_applying_deltas();
        (Painted::measure(shapes), delay)
    }

    /// The pointer events of one press or release on `at`.
    fn press(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
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

    const WINDOW_W: f32 = 1400.0;
    const WINDOW_H: f32 = 900.0;

    /// The stories the window is showing, by id — what the page's list is built from.
    fn shown(atlas: &Atlas) -> Vec<i64> {
        atlas.snapshot.articles.iter().map(|row| row.id).collect()
    }

    /// Press and release the rail's `label`, then paint the frame it asked for.
    ///
    /// The third frame is the one the fix promises: no second press, and the frame the press
    /// requested is where the rail's ink and the status row are current.
    fn press_rail(
        ctx: &egui::Context,
        atlas: &mut Atlas,
        label: &str,
    ) -> (Painted, Painted, std::time::Duration) {
        let before = frame_asking(ctx, atlas, Vec::new()).0;
        let at = before.rail_run(label).unwrap_or_else(|| {
            panic!("the rail paints no `{label}`; it paints {:?}", before.rail_texts())
        });
        let reads_before = atlas.reads;
        frame_asking(ctx, atlas, press(at, true));
        let (release, asked) = frame_asking(ctx, atlas, press(at, false));
        assert!(atlas.reads > reads_before, "`{label}` asked the archive for nothing");
        let (next, _) = frame_asking(ctx, atlas, Vec::new());
        (release, next, asked)
    }

    #[test]
    fn the_24_hour_chip_reads_its_window_and_the_page_says_so() {
        let mut atlas = Atlas::with_archive(archive_with_three_windows("chip-24"));
        let ctx = egui::Context::default();
        let (start, _) = frame_asking(&ctx, &mut atlas, Vec::new());
        assert_eq!(shown(&atlas), vec![1, 2], "the 3 d list, newest first");
        assert_eq!(atlas.snapshot.hours, 72);
        assert!(
            start.page_texts().iter().any(|text| *text == "DEU 2"),
            "the page does not paint the 3 d window's own count: {:?}",
            start.page_texts()
        );
        assert!(
            status_line(&start).contains("2 stories · 3 d window"),
            "the status row does not state the window it read: {:?}",
            status_line(&start)
        );

        let reads_before = atlas.reads;
        let (_release, next, _) = press_rail(&ctx, &mut atlas, "24 h");
        assert_eq!(atlas.hours, 24, "the chip did not set the window");
        assert_eq!(atlas.snapshot.hours, 24, "the read still answered the old window");
        assert_eq!(shown(&atlas), vec![1], "the 24 h list is not the 24 h rows");
        assert_eq!(
            atlas.reads,
            reads_before + 1,
            "one press is one read, not {}",
            atlas.reads - reads_before
        );
        // The page the reader is looking at, on the frame the press asked for: the 30 h story
        // is gone from the list, and the status row states the new window, the new count and
        // the new read.
        assert!(
            next.page_texts().iter().any(|text| *text == "DEU 1"),
            "the page still paints the 3 d window's count: {:?}",
            next.page_texts()
        );
        let line = status_line(&next);
        assert!(line.contains("1 story · 24 h window"), "the row did not follow the read: {line:?}");
        assert!(
            line.contains(&format!("{} ", atlas.reads)) && line.contains("read"),
            "the row does not carry the fresh read stamp: {line:?}"
        );
        // And the rail marks the chip in force, in ink (there is no fill to measure).
        assert_eq!(
            next.rail_ink("24 h"),
            Some(theme::INK),
            "the chip that is on is not painted in ink"
        );
        assert_eq!(
            next.rail_ink("3 d"),
            Some(theme::DIM),
            "the chip that is off is not dimmed"
        );
    }

    #[test]
    fn the_seven_day_chip_widens_the_window_back() {
        let mut atlas = Atlas::with_archive(archive_with_three_windows("chip-7d"));
        let ctx = egui::Context::default();
        frame_asking(&ctx, &mut atlas, Vec::new());
        // Start at 24 h so the press widens rather than narrows.
        let (_, _, _) = press_rail(&ctx, &mut atlas, "24 h");
        assert_eq!(shown(&atlas), vec![1]);
        let reads_before = atlas.reads;
        let (_, next, _) = press_rail(&ctx, &mut atlas, "7 d");
        assert_eq!((atlas.hours, atlas.snapshot.hours), (168, 168));
        assert_eq!(shown(&atlas), vec![1, 2, 3], "the 7 d list is not the 7 d rows");
        assert_eq!(atlas.reads, reads_before + 1, "one press is one read");
        assert!(
            next.page_texts().iter().any(|text| *text == "DEU 3"),
            "the page does not paint the 7 d window's own count: {:?}",
            next.page_texts()
        );
        assert!(
            status_line(&next).contains("3 stories · 7 d window"),
            "the row did not follow the read: {:?}",
            status_line(&next)
        );
    }

    #[test]
    fn refresh_reads_again_and_the_status_row_stamps_it() {
        let mut atlas = Atlas::with_archive(archive_with_three_windows("refresh"));
        let ctx = egui::Context::default();
        let (start, _) = frame_asking(&ctx, &mut atlas, Vec::new());
        let reads_before = atlas.reads;
        let (release, next, _) = press_rail(&ctx, &mut atlas, "Refresh");
        assert_eq!(
            (atlas.hours, atlas.snapshot.hours),
            (72, 72),
            "Refresh changed the window"
        );
        assert_eq!(shown(&atlas), vec![1, 2], "Refresh changed the list");
        assert_eq!(
            atlas.reads,
            reads_before + 1,
            "Refresh read {} times",
            atlas.reads - reads_before
        );
        // The page says it read: the row's stamp is in the frame the press asked for.
        assert!(
            status_line(&release).contains(&format!("{} reads", atlas.reads)),
            "the release frame still shows the old read count: {:?} (now {})",
            status_line(&release),
            atlas.reads
        );
        assert_ne!(status_line(&release), status_line(&start));
        assert!(
            status_line(&next).contains(&format!("{} reads", atlas.reads)),
            "{:?}",
            status_line(&next)
        );
    }

    #[test]
    fn a_chip_press_repaints_on_the_next_frame_and_nothing_polls() {
        let mut atlas = Atlas::with_archive(archive_with_three_windows("asking"));
        // Monitors, because it is the one page that animates nothing (measured: a settled
        // quiet frame there asks for `Duration::MAX`). On Overview and Settings a quiet frame
        // asks for another of its own — the map, the briefing card and the settings form do —
        // and that background would hide both halves of this test: that a press asks for its
        // frame, and that nothing afterwards does.
        atlas.page = Page::Monitors;
        let ctx = egui::Context::default();
        // The first two frames of a window set its fonts and its layout, and ask for the
        // frames that show them; from the third on, a frame with no input asks for nothing.
        for _ in 0..2 {
            frame_asking(&ctx, &mut atlas, Vec::new());
        }
        let (_, idle) = frame_asking(&ctx, &mut atlas, Vec::new());
        assert_eq!(
            idle,
            std::time::Duration::MAX,
            "a quiet frame on Monitors asks the window system to paint again: it polls"
        );
        let reads_before = atlas.reads;
        let (_, next, asked) = press_rail(&ctx, &mut atlas, "24 h");
        assert_eq!(
            asked,
            std::time::Duration::ZERO,
            "the chip press did not ask for the frame that marks it"
        );
        assert_eq!(
            next.rail_ink("24 h"),
            Some(theme::INK),
            "the frame the press asked for does not mark the window in force"
        );
        // That one frame is the only one: the quiet frames after it ask for nothing, and no
        // frame reads the archive on its own — the rail is a control, not a poll.
        let reads_after = atlas.reads;
        for _ in 0..6 {
            let (_, quiet) = frame_asking(&ctx, &mut atlas, Vec::new());
            assert_eq!(
                quiet,
                std::time::Duration::MAX,
                "a quiet frame asks for another: the rail polls"
            );
        }
        assert_eq!(atlas.reads, reads_after, "quiet frames read the archive");
        assert_eq!(atlas.reads, reads_before + 1, "one press, one read");
    }

}
