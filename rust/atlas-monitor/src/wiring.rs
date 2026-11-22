//! `wiring` — how a service is built out of this machine's parts.
//!
//! Decisions: which seams the real thing is made of (`atlas-ingest`'s pipeline for
//! research, `atlas-ai`'s client for the two questions, `atlas-store`'s clock), and
//! what a check is allowed to cost ([`Settings`]).
//!
//! It does not: run a check. Everything here is construction, so a test can build
//! the same [`MonitorService`] with fakes and nothing else has to change.
//!
//! [`MonitorService::native`] **fails** when a rule engine `atlas-ingest` needs is
//! not built ([`crate::error::Error`] naming the binary), because a check that
//! silently does no research is worse than a refusal: it would report "nothing new"
//! for a query nothing ever searched for.

use std::sync::Arc;

use atlas_store::Store;

use crate::enrich::{Enrich, IngestEnrich, NoEnrich};
use crate::error::Result;
use crate::judge::{Judge, ModelJudge, QueryTermsJudge};
use crate::research::{IngestResearch, NoResearch, Research};
use crate::schedule::{Clock, SystemClock};
use crate::service::MonitorService;
use crate::{MAX_CANDIDATES, MAX_JUDGED, RESEARCH_STEPS};

/// How much one check may do. Defaults are the module constants; a deployment (or
/// a test) can lower them without touching the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Candidate articles selected per check.
    pub max_candidates: usize,
    /// Articles actually judged per check (one or two model calls each).
    pub max_judged: usize,
    /// Model turns a research pass may take.
    pub research_steps: usize,
    /// The lookback when a monitor has never been checked. `None` means the
    /// archive's own window (`Store::window_hours`, 72 by default) — the 72-hour
    /// rule stays in `atlas-store`.
    pub window_hours: Option<i64>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            max_candidates: MAX_CANDIDATES,
            max_judged: MAX_JUDGED,
            research_steps: RESEARCH_STEPS,
            window_hours: None,
        }
    }
}

impl MonitorService {
    /// The production wiring: `atlas-ingest`'s pipeline for research and `atlas-ai`'s
    /// client for the two questions, both taken from this machine's configuration.
    ///
    /// Fails when the rule engines `atlas-ingest` needs are not built — it never
    /// degrades to "no research", because a silent no-op check is worse than a
    /// refusal that names the command to fix it.
    pub fn native(store: Arc<Store>, root: impl AsRef<std::path::Path>, clock: Arc<dyn Clock>) -> Result<MonitorService> {
        Ok(native_with_research(store, root, clock)?.service)
    }

    /// The wiring for a caller that only reads (a list, an alert page, a status
    /// line): no research pass and no model.
    pub fn reader(store: Arc<Store>) -> MonitorService {
        MonitorService::new(
            store,
            Arc::new(NoResearch::default()),
            Arc::new(crate::judge::NoJudge::new("this caller does not judge")),
            Arc::new(SystemClock::new()),
            Settings::default(),
        )
    }
}

/// A check service **and** the two seams it keeps private, for the caller that drives
/// research passes of its own: the agent's serve loop.
///
/// [`MonitorService::native`] hides its research seam behind a check, because a check is
/// the only thing the CLI ever runs. The loop is a second caller: a `research` request is
/// a pass with no monitor and no judging, and an `enrich` request is a pass with no model
/// question at all. One pipeline, one client and one store are built here and shared, so
/// the agent and the window's checks cannot end up talking to two different pipelines.
pub struct Native {
    /// The check service (monitors, alerts, the check log).
    pub service: MonitorService,
    /// The research pass, unjudged: what a `research`/`topic_add` request runs.
    pub research: Arc<dyn Research>,
    /// The enrichment pass: what an `enrich` request runs.
    pub enrich: Arc<dyn Enrich>,
    /// The briefing pass: what a `brief` request runs and what the loop's bounded
    /// refresh calls after a run that stored something.
    pub briefs: Arc<dyn crate::brief::Briefs>,
}

/// The briefing seam for a build that has one.
///
/// One call into `atlas_ingest::Pipeline::brief` (which writes the row) and one into
/// `atlas_store::Store::briefing_get` (which says what the cached row had read), both
/// behind [`crate::brief::Briefs`] so the loop is testable without a model.
pub fn briefs_for(
    store: Arc<Store>,
    pipeline: Arc<atlas_ingest::Pipeline>,
) -> Arc<dyn crate::brief::Briefs> {
    Arc::new(crate::brief::PipelineBriefs::new(store, pipeline))
}

/// The production wiring, with the seams the serve loop drives directly.
pub fn native_with_research(
    store: Arc<Store>,
    root: impl AsRef<std::path::Path>,
    clock: Arc<dyn Clock>,
) -> Result<Native> {
    let ingest_config = atlas_ingest::Config::for_root(&root);
    let pipeline = Arc::new(atlas_ingest::Pipeline::native(Arc::clone(&store), ingest_config)?);
    let research: Arc<dyn Research> = Arc::new(IngestResearch::new(Arc::clone(&pipeline)));
    let enrich = Arc::new(IngestEnrich::new(Arc::clone(&pipeline)));

    // The reader's three numbers, from the same `data/config.json` the launcher reads: the
    // window llama.cpp was started with, the thought the model may spend and the answer it may
    // write. A hand-edited file outside a range is clamped with a note; a server that reports a
    // smaller window than the config asks for is obeyed, with a note, instead of being trimmed
    // to in silence.
    let paths = atlas_config::Paths::at(root.as_ref());
    let reader = atlas_config::Config::load_at(&paths)
        .unwrap_or_else(|_| atlas_config::Config::build(&paths, &[], &serde_json::Map::new()));
    let (configured, mut budget_notes) = atlas_ai::Budgets::from_config(&reader);

    let judge: Arc<dyn Judge> = match atlas_ai::chat::HttpLlamaClient::new(
        &atlas_ingest::endpoint_string(),
        &atlas_ingest::model_id(),
        atlas_ai::DEFAULT_TIMEOUT,
    ) {
        Ok(client) => {
            // One probe, at construction: what the server says its window is. A server that
            // does not answer `/props` is not guessed at.
            let reported = atlas_ai::LlamaClient::probe(&client)
                .ok()
                .and_then(|info| info.context_size);
            let (budgets, smaller) = configured.against(reported);
            budget_notes.extend(smaller);
            let shared = Arc::new(client) as Arc<dyn atlas_ai::LlamaClient + Send + Sync>;
            Arc::new(ModelJudge::with_budgets(shared, budgets))
        }
        Err(error) => Arc::new(crate::judge::NoJudge::new(format!(
            "the local model endpoint was refused: {}",
            error
        ))),
    };

    let mut service = MonitorService::new(
        Arc::clone(&store),
        Arc::clone(&research),
        judge,
        clock,
        Settings::default(),
    );
    for note in budget_notes {
        // Every check this service runs carries the sentence: a number the reader set and the
        // app could not obey is not something to discover from a missing answer.
        service = service.with_note(note);
    }
    Ok(Native {
        service,
        research,
        enrich,
        briefs: briefs_for(Arc::clone(&store), Arc::clone(&pipeline)),
    })
}

/// The wiring for a caller that only reads and never works the queue: the bridge, a
/// status line, this crate's own one-shot CLI.
///
/// Every seam refuses, so a caller that mistakes it for a working agent gets a sentence
/// (and a `finish` with that sentence) rather than a silent no-op.
pub fn idle(store: Arc<Store>, clock: Arc<dyn Clock>) -> Native {
    let research: Arc<dyn Research> = Arc::new(NoResearch::new(
        "this process is not the monitoring agent",
    ));
    Native {
        service: MonitorService::new(
            Arc::clone(&store),
            Arc::clone(&research),
            Arc::new(crate::judge::NoJudge::new("this process does not judge")),
            clock,
            Settings::default(),
        ),
        research,
        enrich: Arc::new(NoEnrich::default()),
        briefs: Arc::new(crate::brief::NoBrief::default()),
    }
}

/// The judge a service falls back to when the model cannot be asked: the monitor's
/// own query terms. Named here so a caller can say what it wired — and it is now only
/// ever wired by hand (`MonitorService::with_fallback`). Term scoring is a decision for
/// the caller, never the default: a check whose articles the model did not judge leaves
/// them unjudged and says so in its note.
pub fn fallback_judge() -> Arc<dyn Judge> {
    Arc::new(QueryTermsJudge)
}

/// The clock a production service reads.
pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock::new())
}

/// A service wired for real, from a store and a project root.
pub fn native(store: Arc<Store>, root: impl AsRef<std::path::Path>) -> Result<MonitorService> {
    MonitorService::native(store, root, system_clock())
}
