//! The three operations, and the evidence that comes with them.
//!
//! * [`Search::web_search`] — the model's query to the engine's web surface, in
//!   relevance order. This is the tool `atlas-ai` calls when the story is not in
//!   the feeds yet.
//! * [`Search::news_for`] — the engine's **news** surface, keeping only what
//!   `atlas-dates` reads as a timestamp inside the window the caller passed
//!   (`since`, epoch seconds). The caller decides the window; this crate applies
//!   it.
//! * [`Search::find_news`] — the same thing said in hours, for the tool shape
//!   `atlas-ingest` already speaks.
//!
//! All three answer with a JSON object in the shape the rest of the app already
//! reads (`query`, `engine`, `count`, `results`, `note`, `refused_links`) plus
//! the evidence a model needs to judge what it got: which surface answered, how
//! many links the page held, how many were the engine's own navigation, how many
//! the gate refused, how many were duplicates, how many carried no readable date
//! (`undated`), how many were older than the window (`out_of_window`), how many the
//! per-call date budget never reached (`not_checked` — not the same thing as
//! undated, and the note says which), whether the relevance order changed anything,
//! and how long it took.
//!
//! Nothing here filters by publisher, host or vocabulary. Ranking is
//! [`crate::rank`] (cosine against the query, `atlas-core`'s primitive); the only
//! filter that can drop a link is the safety gate, and it is counted.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};

use crate::clock::{Clock, System};
use crate::engine::{Engine, Surface};
use crate::extract;
use crate::fetch::Fetcher;
use crate::rank;
use crate::rules::Rules;
use crate::{cards, html};

/// The window `find_news` may ask for, in hours — the app's current-reporting
/// window, so a tool call cannot ask this machine for a week of archives.
pub const MAX_HOURS: i64 = 72;

/// What one call is: which surface, how many results, which date rule.
#[derive(Debug, Clone, Copy)]
struct Ask {
    surface: Surface,
    limit: usize,
    /// The caller's window in seconds; 0 means "no window, show the date".
    max_age_s: i64,
    since: i64,
}

/// One configured tool: an engine, the rules, a transport and a clock.
pub struct Search {
    engine: Engine,
    rules: Arc<dyn Rules>,
    fetcher: Arc<dyn Fetcher>,
    clock: Arc<dyn Clock>,
}

impl Search {
    /// The tool, on the machine's clock.
    pub fn new(engine: Engine, rules: Arc<dyn Rules>, fetcher: Arc<dyn Fetcher>) -> Search {
        Search {
            engine,
            rules,
            fetcher,
            clock: Arc::new(System),
        }
    }

    /// The same tool with a fixed clock (tests, one reproducible measurement).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Search {
        self.clock = clock;
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn rules(&self) -> &Arc<dyn Rules> {
        &self.rules
    }

    /// The model's query against the web surface, ranked by relevance.
    pub fn web_search(&self, query: &str, max_results: usize) -> Value {
        let limit = clamp_limit(max_results, crate::DEFAULT_RESULTS);
        self.ask(
            query,
            Ask {
                surface: Surface::Web,
                limit,
                max_age_s: 0,
                since: 0,
            },
        )
    }

    /// The news surface, only what is dated inside `since .. now`.
    pub fn news_for(&self, query: &str, since: i64, limit: usize) -> Value {
        let limit = clamp_limit(limit, crate::DEFAULT_RESULTS);
        let now = self.clock.now();
        // An arithmetic guard, not a policy: the caller's window is used as
        // given, but a nonsense `since` cannot overflow the subtraction.
        let max_age_s = now.saturating_sub(since).clamp(1, MAX_HOURS * 86_400 * 10);
        self.ask(
            query,
            Ask {
                surface: Surface::News,
                limit,
                max_age_s,
                since,
            },
        )
    }

    /// [`Search::news_for`] said in hours: the shape `atlas-ingest` calls.
    pub fn find_news(&self, query: &str, hours: i64, limit: usize) -> Value {
        let hours = if hours <= 0 { 48 } else { hours.min(MAX_HOURS) };
        let since = self.clock.now() - hours * 3_600;
        let mut answer = self.news_for(query, since, limit);
        if let Value::Object(map) = &mut answer {
            map.insert("hours".to_string(), json!(hours));
        }
        answer
    }

    /// The one path all three operations take.
    fn ask(&self, query: &str, plan: Ask) -> Value {
        let query = crate::untrusted(query, crate::MAX_QUERY_CHARS);
        if query.is_empty() {
            return json!({
                "error": "query required - pass the words to search for, e.g. 'chip export rules'",
                "results": [],
            });
        }
        let engine_host = self.engine.host(plan.surface);
        let url = self.engine.url(plan.surface, &query);
        let started = Instant::now();
        let answer = self.fetcher.fetch(&url, crate::MAX_PAGE_BYTES);
        let now = self.clock.now();

        if !answer.usable() {
            let mut payload = self.evidence(&query, plan, &engine_host);
            payload["error"] = json!(format!(
                "the search engine did not answer: {}",
                extract::reason_field(&answer)
            ));
            payload["results"] = json!([]);
            payload["ms"] = json!(elapsed_ms(started));
            return payload;
        }

        let markup = html::scan(&answer.body);
        let page = cards::cards(&markup);
        let outcome = extract::extract(&page, &url, &engine_host, &*self.rules);
        let found = outcome.candidates.len();
        let (parsed, navigation, refused, duplicates) = (
            outcome.parsed,
            outcome.navigation,
            outcome.refused,
            outcome.duplicates,
        );

        // The news surface is asked for *current* reporting, so the window is
        // applied to everything the page offered, before anything is ranked.
        let windowed = if plan.max_age_s > 0 {
            extract::keep_current(outcome.candidates, &*self.rules, now, plan.max_age_s)
        } else {
            extract::Windowed {
                kept: outcome.candidates,
                ..extract::Windowed::default()
            }
        };
        let not_asked = windowed.not_asked;
        let undated = windowed.undated;
        let out_of_window = windowed.out_of_window;
        let candidates = windowed.kept;

        let texts: Vec<String> = candidates
            .iter()
            .map(|candidate| format!("{} {}", candidate.hit.title, candidate.hit.snippet))
            .collect();
        let ranks: Vec<usize> = candidates
            .iter()
            .map(|candidate| candidate.hit.page_rank)
            .collect();
        let scores = rank::scores(&query, &texts);
        let order = rank::order(&scores, &ranks);
        let ranked = order
            .iter()
            .enumerate()
            .any(|(position, index)| position != *index);
        // `order` holds indices into `candidates`, so the score is carried across
        // by position rather than looked up by page rank.
        let mut ordered: Vec<extract::Candidate> = Vec::with_capacity(order.len());
        for index in order.iter() {
            if let Some(candidate) = candidates.get(*index) {
                let mut candidate = candidate.clone();
                candidate.hit.score = scores.get(*index).copied().unwrap_or(0.0).max(0.0);
                ordered.push(candidate);
            }
        }
        ordered.truncate(plan.limit);
        // Only the rows that leave this function pay for a date or a query-string
        // call: on a web surface forty cards with no date cost nothing.
        let mut undated = undated;
        if plan.max_age_s == 0 {
            extract::fill_shown_dates(&mut ordered, &*self.rules, now);
            // On a web surface nothing is filtered by date, so "undated" counts
            // the rows that were handed back without one.
            undated = ordered
                .iter()
                .filter(|candidate| !candidate.hit.dated())
                .count();
        }        for candidate in ordered.iter_mut() {
            candidate.hit.url = extract::untracked(&candidate.hit.url, &*self.rules);
        }
        let count = ordered.len();
        let results: Vec<Value> = ordered.iter().map(|candidate| candidate.hit.to_json()).collect();

        let mut payload = self.evidence(&query, plan, &engine_host);
        payload["hit_count"] = json!(found);
        payload["count"] = json!(count);
        payload["results"] = json!(results);
        payload["parsed_links"] = json!(parsed);
        payload["navigation"] = json!(navigation);
        payload["refused_links"] = json!(refused);
        payload["duplicates"] = json!(duplicates);
        payload["undated"] = json!(undated);
        payload["out_of_window"] = json!(out_of_window);
        payload["not_checked"] = json!(not_asked);
        payload["ranked"] = json!(ranked);
        payload["status"] = json!(answer.status);
        payload["page_bytes"] = json!(answer.body.len());
        payload["truncated"] = json!(answer.truncated);
        payload["ms"] = json!(elapsed_ms(started));
        payload["note"] = json!(note_for(
            plan,
            count,
            undated,
            out_of_window,
            not_asked,
            refused,
            answer.status
        ));
        payload
    }

    fn evidence(&self, query: &str, plan: Ask, engine_host: &str) -> Value {
        let mut payload = json!({
            "query": query,
            "engine": engine_host,
            "surface": plan.surface.name(),
            "results": [],
            "count": 0,
        });
        if plan.surface == Surface::News {
            payload["since"] = json!(plan.since);
            payload["window_s"] = json!(plan.max_age_s);
        }
        payload
    }
}

fn clamp_limit(requested: usize, fallback: usize) -> usize {
    match requested {
        0 => fallback,
        n => n.min(crate::MAX_RESULTS),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// The sentence the model reads last, and the one it acts on.
fn note_for(
    plan: Ask,
    count: usize,
    undated: usize,
    out_of_window: usize,
    not_asked: usize,
    refused: usize,
    status: u16,
) -> String {
    if count == 0 && status != 200 {
        // A search endpoint that answers "accepted" and no list is a challenge
        // page, not an empty search: saying so is the difference between the
        // model rephrasing its query and the reader changing the engine.
        return format!(
            "the engine answered {} without a result list - that is a challenge or rate-limit page, not an empty search; try again, or configure another engine template",
            status
        );
    }
    if count == 0 {
        return match plan.surface {
            Surface::Web => {
                "the engine returned nothing for those words - try shorter or more concrete words, or a name or a place".to_string()
            }
            Surface::News => format!(
                "nothing dated inside the window came back ({} undated, {} outside it{}) - widen the window, or call web_search",
                undated,
                out_of_window,
                if not_asked > 0 {
                    format!(", {} not read because the date budget ran out", not_asked)
                } else {
                    String::new()
                }
            ),
        };
    }
    let mut note = String::from(
        "open the most relevant 2-4 with read_article; you decide what is relevant",
    );
    if undated > 0 && plan.surface == Surface::News {
        note.push_str(&format!(
            "; {} more links carried no readable date and were left out",
            undated
        ));
    }
    // `not_asked` is not "undated": the page carried more date labels than one call
    // may read, so those cards were never examined. A model that is deciding whether
    // the surface dates its results needs the difference.
    if not_asked > 0 {
        note.push_str(&format!(
            "; {} more links were not read (a call reads at most {} date labels) - narrow the query if the window matters",
            not_asked,
            crate::extract::MAX_DATE_LOOKUPS
        ));
    }
    if refused > 0 {
        note.push_str(&format!(
            "; {} link(s) were refused by the url gate and left out",
            refused
        ));
    }
    note
}
