//! atlas-monitor — topic monitoring, in Rust.
//!
//! This crate is the port of `server/monitor.py` (521 lines). A **monitor** is a
//! saved query the user owns: a topic, a query, a threshold, a cadence and an
//! active flag. A **check** is one explicit pass for one monitor; an **alert** is
//! what a check leaves behind when an article passes the threshold.
//!
//! # The rule this crate exists for
//!
//! Nothing here polls a feed and nothing crawls. A check happens because
//! * the user pressed a button ([`Reason::Button`]),
//! * the scheduler asked at the monitor's cadence ([`Reason::Scheduler`]), or
//! * the local model called the monitor tool ([`Reason::Tool`]).
//!
//! Every check writes one row to the check log with the reason it ran, including
//! the checks that were refused. There is no timer thread in this crate: the
//! [`Scheduler`] answers "who is due at this instant" and the server decides when
//! to ask, which is what makes a fake clock a test and not a rewrite.
//!
//! # What one check does
//!
//! 1. Takes the single-flight claim for that monitor — a second check of the same
//!    monitor while one is running is refused, not queued
//!    ([`InFlight`]).
//! 2. Calls [`atlas_ingest`]'s research pipeline for the monitor's query: the
//!    model searches, reads, classifies and stores articles through its tools.
//!    This crate never searches and never stores an article itself.
//! 3. Selects what is new since the monitor's last check — the ids the research
//!    pass just stored, plus articles fetched since then ([`candidates`]).
//! 4. Asks the local model to judge each candidate: is this reporting, and how
//!    relevant is it to the monitor's topic? The verdict carries the score, the
//!    terms the model chose and a short reason. A question the model cannot answer
//!    is asked once more (`judge::worth_asking_again`) and then reported: the
//!    check ends `partial`, the reason reaches the check log, and the alert says
//!    that the query's own terms decided instead.
//! 5. Raises an alert above the threshold, remembers a below-threshold verdict so
//!    the next check does not pay for it again, and never stores two alerts for
//!    one article ([`alert`]).
//! 6. Records `last_run`, the hit count, and the check log row.
//!
//! # Where the decisions live
//!
//! | Decision | Owner |
//! | --- | --- |
//! | is this page news reporting | `atlas-ai`'s `NEWS_PROMPT` (`prompts::news_prompt`) |
//! | relevance to the topic, and the terms behind it | this crate's [`prompt`], answered by the model |
//! | may this URL be opened, is this text safe to store | `atlas-guard` |
//! | the articles, the alerts, the check log | `atlas-store` (`monitors`) |
//! | what a check stored | `atlas-ingest` |
//! | the freshness window, the timestamps | `atlas-store`'s clock (72 h default) |
//!
//! # No vocabulary, ever
//!
//! This crate holds no list of news words, publishers or hosts. The relevance
//! question goes to the local model, and a judgement is a *bounded turn*
//! ([`atlas_ai::judge`]): think with room to answer, rescue once without reasoning,
//! read the object out of prose if that is what it wrote. When the model still does
//! not answer, nothing is scored — the check is `partial`, the log says which articles
//! went unjudged, and they stay in the archive for the reader. The one non-model path
//! — [`QueryTermsJudge`] — reads the terms out of the **monitor's own query** and is
//! wired explicitly by a caller ([`MonitorService::with_fallback`]), never by default:
//! measured, run `monitor_runs` id 1 stored `scorer=query-terms` for all 8 articles
//! because a starved call returned an empty answer, and the reader was told nothing.
//! What term scoring cannot answer is "is this news": without a model nothing is
//! filtered, because an article the reader can see beats a silent, wrong rejection.
//!
//! # The agent: `serve`
//!
//! The window cannot ingest — it writes no file and starts no process — so a click on
//! "monitor this topic" is a row in `data/requests.db` ([`atlas_store::requests`]) and
//! [`serve::Serve`] is what acts on it: claim one request, run it (a research pass, a
//! monitor check, a new monitor, an enrichment pass), `finish` it, then run the monitors
//! that are due. That loop is the whole of "24/7": the cadence lives on the monitor row,
//! so it survives a restart, and the loop asks [`Scheduler::due`] at every iteration.
//!
//! It decides nothing about content: the payload carries the user's sentence, the model
//! chooses what to search and which pages to read, and the loop times the action and
//! prints one JSON line about it.
//!
//! # What may never be logged
//!
//! Article text, summaries and prompts stay out of the check log and out of every
//! error message. Alerts carry a title and the model's reason because the reader
//! asked for them; the log carries counts, ids, the reason a check ran, and — when
//! the model refused a page or never answered — the model's own sentence, so a
//! monitor that stays quiet can be explained. It never carries the article.
//!
//! # Local only
//!
//! Nothing here binds a socket, downloads a file or resolves a name. The model is
//! reached through `atlas-ai`, which refuses anything but loopback.
//!
//! # Wiring it
//!
//! ```no_run
//! use std::sync::Arc;
//! use atlas_monitor::{MonitorService, Reason, Scheduler, SystemClock, spec::Draft};
//! use atlas_store::Store;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Arc::new(Store::open("data/atlas.db")?);
//! store.init()?;
//! let service = MonitorService::native(Arc::clone(&store), ".", Arc::new(SystemClock::new()))?;
//! let id = service.add(&Draft::parse("Chips", "chip export rules", None, None, None)?)?;
//! let report = service.run(id, Reason::Button);
//! println!("{} new alerts", report.new_alerts());
//! let tick = service.tick(true)?;            // what the scheduler would do now
//! assert!(tick.enabled || tick.ran.is_empty());
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod alert;
pub mod brief;
pub mod candidates;
pub mod enrich;
pub mod error;
pub mod import_model;
pub mod judge;
pub mod prompt;
pub mod research;
pub mod run;
pub mod schedule;
pub mod serve;
pub mod service;
pub mod settings_set;
pub mod spec;
pub mod stop;
pub mod terms;
pub mod verdict;
pub mod wire;
pub mod wiring;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod serve_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod tests;

pub use alert::{Alert, Why};
pub use brief::{Ask, Briefs, Fresh, NoBrief, Visit, BRIEF_REFRESH};
pub use candidates::{Article, Selected};
pub use enrich::{Enrich, EnrichOutcome, IngestEnrich, NoEnrich};
pub use error::{Error, Result};
pub use judge::{Judge, Judgement, ModelJudge, NoJudge, QueryTermsJudge};
pub use research::{IngestResearch, NoResearch, Research, ResearchOutcome};
pub use run::{InFlight, ResearchSummary, RunReport, RunStatus};
pub use schedule::{Clock, FixedClock, Scheduler, SystemClock, Tick};
pub use serve::{Acted, Pass, Report, Serve};
pub use stop::Stop;
pub use service::MonitorService;
pub use wiring::Settings;
pub use spec::{Draft, Monitor, Reason};
pub use verdict::{Scorer, Verdict};

/// The most candidates one check judges. `monitor.py::MAX_ARTICLES` was 25 for
/// the candidate scan; asking the model about 25 articles takes minutes on a local
/// model, so the judged set is smaller and the rest wait for the next check.
pub const MAX_JUDGED: usize = 8;

/// The most candidate articles one check selects at all.
pub const MAX_CANDIDATES: usize = 25;

/// How many model turns one research pass may take
/// (`atlas_ingest::research::DEFAULT_MAX_STEPS` was 14).
pub const RESEARCH_STEPS: usize = 14;

/// "Check all" stops starting new checks after this long, so one click stays
/// bounded (`monitor.py::ALL_BUDGET_SEC`).
pub const ALL_BUDGET_SEC: u64 = 120;
