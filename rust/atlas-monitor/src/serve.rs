//! `serve` — the always-on agent: claim a request, run it, sleep, repeat.
//!
//! This is the half of the product the window cannot be. The window writes no file and
//! starts no process — its archive handle is read-only — so a click on "monitor this
//! topic" can only be *stored*; `data/requests.db` ([`atlas_store::requests`]) is where it
//! lands, and this loop is what turns it into work. It is also what keeps a monitor's
//! cadence while nobody is looking, which is what "24/7" means here.
//!
//! # One iteration
//!
//! 1. **Claim one request** — `claim_due(now, 600 s, 1)`. One, because the model server
//!    has one slot (`-np 1`): claiming what cannot run is a lease that expires while the
//!    row waits, and a queue that looks busy while it is not. A row whose lease expired
//!    (a child that died holding it) is claimed again — that is the stale case, and the
//!    only case where a row is claimed twice.
//! 2. **Run it** and `finish(id, error)` — always, success or failure. A finished row is
//!    never claimed again, whether it worked or not: a failed pass is a fact, and the next
//!    pass is a new request, not a second run of the same row. A pass that stored at least
//!    one story then refreshes the reader's briefings, at most
//!    [`crate::brief::BRIEF_REFRESH`] model calls, in the order
//!    [`crate::brief::refresh_after`] documents — window, busiest countries, the run's own
//!    scope. A briefing that could not be written is one line on stderr; it never fails the
//!    run the reader asked for.
//! 3. **Run the monitors that are due** — [`MonitorService::tick`] answers who is due at
//!    this instant, and each one runs one research pass with [`Reason::Scheduler`], so the
//!    check log says why it ran. This is the 24/7 part, and it refreshes briefings by the
//!    same rule.
//! 4. **Sleep** — 5 s while the queue still has work, 20 s when it is empty, and never
//!    longer than the next monitor's due time. Never zero: a monitor whose run failed
//!    leaves its cadence due, and a zero-length sleep would be a busy loop around a
//!    failure.
//!
//! # The one action that starts a program
//!
//! Every arm here calls a seam inside this process except one: a `settings_set` row that
//! carries `import_model` starts `atlas-setup --import-model` as a child
//! ([`crate::import_model`]), because naming a model file is the install tool's job and the
//! window can start nothing. It is spawned through `atlas_platform::child` (the environment
//! allowlist, the root as cwd, output inherited), it is waited for while the stop flag is
//! watched, and its answer — the tool's own sentence from `data/setup.json` — is what the
//! row is finished with. One attempt, no retry, no deadline.
//!
//! # The line
//!
//! One JSON object per action on stdout, and nothing else on stdout, ever.
//!
//! The five keys are always there — `kind`, `id`, `tool`, `ms`, `stored` — and three are
//! added only when they say something: `monitor` (what the action was about), `alerts`
//! (raised by a check) and `error` (why it failed, one scrubbed sentence). `serde_json`
//! writes a map in key order, so the fields arrive alphabetically; the *set* is the
//! contract, not the order:
//!
//! ```text
//! {"id":7,"kind":"research","ms":41213,"stored":2,"tool":"web_search"}
//! {"alerts":1,"id":null,"kind":"monitor_run","monitor":3,"ms":8123,"stored":1,"tool":"save_article"}
//! {"id":9,"kind":"enrich","ms":1400,"stored":0,"tool":null}
//! {"error":"local model not reachable - start llama.cpp (run.sh) and retry","id":11,"kind":"research","ms":8,"stored":0,"tool":null}
//! ```
//!
//! `tool` is the last tool the model called in that action, `null` when it called none;
//! `stored` is articles this action stored. The launcher parses none of it — a reader
//! does (`jq` it, or `grep '"kind":"research"'`) — which is why the shape is flat, small
//! and stable. The last example is a real line, measured by running this binary against a
//! temporary `data/requests.db` with one `research` row and no model server.
//!
//! # What it never decides
//!
//! No word list, no publisher list, no query of its own. A `research` request carries the
//! sentence to research; the model decides what to search, which pages to read and how to
//! tag them. This file parses a payload, calls a seam, times it and prints a count.

use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas_store::requests::{Request, Requests};
use serde_json::{json, Value};

use crate::schedule::Clock;
use crate::spec::{Draft, Reason};
use crate::wiring::Native;
use crate::RESEARCH_STEPS;

/// The subcommand that runs the loop.
///
/// Spelled here once and read by this crate's `main.rs`, which dispatches on it before
/// `atlas_lib::cli` reads stdin. `atlas-launcher` spells the same word in its own
/// `agent.rs` — it spawns a *program*, not a crate, so the two crates share the word the
/// way they share the binary's name.
pub const SERVE: &str = "serve";

/// The pause while the queue still holds work.
pub const BUSY_WAIT: Duration = Duration::from_secs(5);

/// The pause when the queue is empty and no monitor is close to due.
pub const IDLE_WAIT: Duration = Duration::from_secs(20);

/// The shortest pause, so a monitor whose run failed cannot spin the loop.
pub const MIN_WAIT: Duration = Duration::from_secs(1);

/// The longest a single sleep lasts, so a stop is noticed within a quarter second even in
/// the middle of a twenty-second idle pause.
///
/// The same probe interval the launcher uses while it waits for the window. Four wake-ups
/// a second that read one atomic are not a busy loop; a loop that slept the whole twenty
/// seconds would answer `SIGTERM` only after the launcher's grace period had expired, and
/// be killed for it.
pub const POLL: Duration = Duration::from_millis(250);

/// How old a claim must be before the row is taken again: ten minutes, four times the
/// longest research pass this machine is expected to run.
pub const STALE_AFTER_SECS: i64 = 600;

/// Model turns one research request may take.
pub const MAX_STEPS: usize = RESEARCH_STEPS;

/// The enrichment pass's default size, matching `atlas-ingest enrich`'s own default.
pub const ENRICH_LIMIT: usize = 5;

/// Where the one line per action goes.
///
/// A trait because the loop's tests assert on the lines and a terminal is not a test: the
/// production implementation writes a line to stdout, the test one keeps them.
pub trait Report: Send + Sync {
    fn line(&self, line: &Value);
}

/// The production report: one JSON object, one newline, nothing else.
pub struct Stdout;

impl Report for Stdout {
    fn line(&self, line: &Value) {
        println!("{}", line);
    }
}

/// A report that keeps its lines, for a test that reads them back.
#[derive(Default)]
pub struct Kept(std::sync::Mutex<Vec<Value>>);

impl Kept {
    pub fn new() -> Kept {
        Kept::default()
    }

    pub fn lines(&self) -> Vec<Value> {
        match self.0.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Report for Kept {
    fn line(&self, line: &Value) {
        match self.0.lock() {
            Ok(mut held) => held.push(line.clone()),
            Err(poisoned) => poisoned.into_inner().push(line.clone()),
        }
    }
}

/// What one action did, which is what its line says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Acted {
    /// The last tool the model called, when it called one.
    pub tool: Option<String>,
    /// Wall time from claim to finish.
    pub ms: u64,
    /// Articles stored.
    pub stored: usize,
    /// Alerts raised (a monitor run only).
    pub alerts: usize,
    /// The monitor a run was about, when it was about one.
    pub monitor: Option<i64>,
    /// Why it did not work, when it did not — one scrubbed sentence.
    pub error: Option<String>,
}

/// What one iteration found to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// This many actions ran (a claimed request counts one, each due monitor one).
    Ran(usize),
    /// Nothing was queued and no monitor was due.
    Idle,
}

/// The loop, over one queue and one wired app.
pub struct Serve<'a> {
    queue: &'a Requests,
    native: &'a Native,
    clock: &'a dyn Clock,
    report: &'a dyn Report,
}

impl<'a> Serve<'a> {
    pub fn new(
        queue: &'a Requests,
        native: &'a Native,
        clock: &'a dyn Clock,
        report: &'a dyn Report,
    ) -> Serve<'a> {
        Serve {
            queue,
            native,
            clock,
            report,
        }
    }

    /// One iteration: the claim first (a click is a person waiting), then the monitors.
    ///
    /// Nothing here sleeps, so a test drives it a step at a time with a clock it owns.
    /// There is no stop: use [`Serve::step_while`] for a step that must be interruptible.
    pub fn step(&self) -> Pass {
        self.step_while(&crate::stop::Stop::quiet())
    }

    /// One iteration under a stop the caller owns.
    pub fn step_while(&self, stop: &crate::stop::Stop) -> Pass {
        let mut ran = 0usize;
        ran += self.drain_fast_topics();
        ran += self.claim_and_run(stop);
        ran += self.run_due();
        if ran == 0 {
            Pass::Idle
        } else {
            Pass::Ran(ran)
        }
    }

    /// The loop: one iteration, then a pause, until [`crate::stop::Stop`] says so.
    ///
    /// A lightweight companion thread drains `topic_add` rows every [`POLL`] interval so
    /// adding a topic (or asking the chat agent to add one) materialises in `topics` and
    /// `monitors` immediately even while the model server is busy on a multi-minute
    /// research pass. Research for the new topic is queued as a `monitor_run` row and
    /// picked up by the main loop as soon as the model slot is free.
    ///
    /// `SIGTERM` is answered between actions — the one in flight is finished and its line
    /// is printed — so a stop is never a half-written row. Returns the number of actions
    /// that ran, which the binary prints as its last line.
    pub fn run(&self, stop: &crate::stop::Stop) -> usize {
        if stop.requested() {
            return 0;
        }
        if let Some(path) = self.status_path() {
            atlas_ingest::live::set_idle(&path, self.queued_count(), Some("agent ready"));
        }
        std::thread::scope(|scope| {
            let fast = scope.spawn(|| {
                let mut fast_ran = 0usize;
                while !stop.requested() {
                    fast_ran += self.drain_fast_topics();
                    std::thread::sleep(POLL);
                }
                fast_ran
            });
            let mut ran = 0usize;
            loop {
                if stop.requested() {
                    break;
                }
                if let Pass::Ran(count) = self.step_while(stop) {
                    ran += count;
                }
                if stop.requested() {
                    break;
                }
                self.pause(stop);
            }
            ran + fast.join().unwrap_or(0)
        })
    }

    /// `<root>/data/agent-status.json` when the archive lives on disk, or `None` for
    /// in-memory test stores.
    fn status_path(&self) -> Option<std::path::PathBuf> {
        crate::settings_set::root_of(self.native.service.store())
            .map(|root| atlas_ingest::live::status_path_for_root(&root))
    }

    fn queued_count(&self) -> usize {
        self.queue.depth().unwrap_or(0).max(0) as usize
    }

    /// Fast lane for `topic_add`: create/activate the `topics` + `monitors` rows
    /// immediately (no model call) and queue a `monitor_run` for the research worker.
    ///
    /// Safe to run concurrently with [`Self::claim_and_run`]: it only claims `topic_add`
    /// rows, and `claim_and_run` only claims non-`topic_add` rows.
    pub(crate) fn drain_fast_topics(&self) -> usize {
        let now = atlas_store::clock::iso_from_epoch(self.clock.now_epoch());
        let claimed = match self
            .queue
            .claim_due_kind("topic_add", &now, STALE_AFTER_SECS, 16)
        {
            Ok(claimed) => claimed,
            Err(error) => {
                eprintln!(
                    "atlas-monitor: the request queue did not answer: {}",
                    crate::error::scrub(&error.to_string())
                );
                return 0;
            }
        };
        let mut ran = 0usize;
        for request in claimed {
            let started = Instant::now();
            let (acted, label) = self.apply_topic_add(&request.payload);
            let ms = started.elapsed().as_millis() as u64;
            let acted = Acted { ms, ..acted };
            let _ = self.queue.finish(request.id, acted.error.as_deref());
            self.report.line(&line("topic_add", Some(request.id), &acted));
            if let Some(path) = self.status_path() {
                let note = match (&acted.error, label) {
                    (None, Some(name)) => {
                        format!("added topic \"{name}\" (queued research)")
                    }
                    (Some(err), _) => format!("could not add topic: {err}"),
                    (None, None) => "added topic (queued research)".to_string(),
                };
                atlas_ingest::live::record_note(&path, &note, Some(self.queued_count()));
            }
            ran += 1;
        }
        ran
    }

    /// Materialise one `topic_add` payload into `topics` + `monitors` and enqueue a
    /// `monitor_run` for the slow research worker.
    fn apply_topic_add(&self, raw_payload: &str) -> (Acted, Option<String>) {
        let payload: Value = match serde_json::from_str(raw_payload) {
            Ok(Value::Object(map)) => Value::Object(map),
            _ => {
                return (
                    Acted {
                        error: Some(
                            "a topic_add request needs one JSON object as its payload".to_string(),
                        ),
                        ..Acted::default()
                    },
                    None,
                );
            }
        };
        let name = text_of(&payload, &["name", "topic", "query"]).unwrap_or_default();
        let query = match text_of(&payload, &["query", "topic", "name"]) {
            Ok(query) => query,
            Err(error) => {
                return (
                    Acted {
                        error: Some(error),
                        ..Acted::default()
                    },
                    None,
                );
            }
        };
        let mut draft = match Draft::parse(
            &name,
            &query,
            integer_of(&payload, "threshold"),
            integer_of(&payload, "cadence_minutes"),
            integer_of(&payload, "topic_id"),
        ) {
            Ok(draft) => draft,
            Err(error) => {
                return (
                    Acted {
                        error: Some(error.message().to_string()),
                        ..Acted::default()
                    },
                    None,
                );
            }
        };
        let topic_id = match self.native.service.ensure_topic(&draft.name, &draft.query) {
            Ok(id) => id,
            Err(error) => {
                return (
                    Acted {
                        error: Some(error.message().to_string()),
                        ..Acted::default()
                    },
                    None,
                );
            }
        };
        if draft.topic_id.is_none() {
            draft.topic_id = Some(topic_id);
        }
        let monitor_id = match self.native.service.add_once(&draft) {
            Ok(id) => id,
            Err(error) => {
                return (
                    Acted {
                        error: Some(error.message().to_string()),
                        ..Acted::default()
                    },
                    None,
                );
            }
        };
        let run_payload = json!({ "monitor_id": monitor_id }).to_string();
        if let Err(error) = self.queue.push("monitor_run", &run_payload) {
            return (
                Acted {
                    monitor: Some(monitor_id),
                    error: Some(error.message().to_string()),
                    ..Acted::default()
                },
                Some(draft.name),
            );
        }
        (
            Acted {
                monitor: Some(monitor_id),
                ..Acted::default()
            },
            Some(draft.name),
        )
    }

    /// Wait for the next iteration, in [`POLL`] slices, and give up as soon as a stop
    /// arrives.
    fn pause(&self, stop: &crate::stop::Stop) {
        let mut left = self.next_wait();
        while !left.is_zero() {
            if stop.requested() {
                return;
            }
            let slice = left.min(POLL);
            std::thread::sleep(slice);
            left -= slice;
        }
    }

    /// How long to pause before the next iteration.
    ///
    /// Measured from the queue's depth and the monitors' cadence, never guessed: 5 s while
    /// work is waiting, 20 s when nothing is, and shorter only when a monitor becomes due
    /// sooner than that. The floor is [`MIN_WAIT`] — a failed run leaves its cadence due,
    /// and a zero-length pause there would be the busy loop this loop must never have.
    pub fn next_wait(&self) -> Duration {
        let queued = self.queue.depth().unwrap_or(0) > 0;
        let base = if queued { BUSY_WAIT } else { IDLE_WAIT };
        let until_due = match self.next_due_in() {
            Some(secs) => Duration::from_secs(secs.max(0) as u64),
            None => base,
        };
        base.min(until_due).max(MIN_WAIT)
    }

    /// Seconds until the next active monitor is due, when the scheduler is on.
    fn next_due_in(&self) -> Option<i64> {
        if !self.native.service.scheduler().enabled() {
            return None;
        }
        let now = self.clock.now_epoch();
        self.native
            .service
            .list()
            .ok()?
            .iter()
            .filter(|monitor| monitor.active && monitor.cadence_minutes > 0)
            .map(|monitor| match monitor.last_run_epoch() {
                None => 0,
                Some(last) => (last + monitor.cadence_minutes.saturating_mul(60) - now).max(0),
            })
            .min()
    }

    /// Claim one due request (excluding `topic_add`, which runs on the fast lane) and run
    /// it. Answers how many actions ran (0 or 1).
    fn claim_and_run(&self, stop: &crate::stop::Stop) -> usize {
        let now = atlas_store::clock::iso_from_epoch(self.clock.now_epoch());
        let claimed = match self
            .queue
            .claim_due_except("topic_add", &now, STALE_AFTER_SECS, 1)
        {
            Ok(claimed) => claimed,
            // A queue that will not answer does not kill the agent — the archive and the
            // model may still be fine, and no row was lost (nothing was claimed) — but it
            // is not silent either: stderr, because stdout carries action lines only, and
            // once per pass, because a queue that is broken stays broken and a flood
            // would be the only thing a reader sees.
            Err(error) => {
                eprintln!(
                    "atlas-monitor: the request queue did not answer: {}",
                    crate::error::scrub(&error.to_string())
                );
                return 0;
            }
        };
        let mut ran = 0usize;
        for request in claimed {
            let started = Instant::now();
            if let Some(path) = self.status_path() {
                let (topic, phase, max_steps) = self.describe_request_start(&request);
                let remaining = self.queued_count().saturating_sub(1);
                let _ = atlas_ingest::live::LiveStatus::start(
                    path,
                    &request.kind,
                    &topic,
                    &phase,
                    max_steps,
                    Some(remaining),
                );
            }
            // What the archive held before the pass: the refresh below reads what it gained
            // from the rows, so no seam has to report ids it did not write.
            let newest = crate::brief::newest_article(self.native.service.store().as_ref());
            let (kind, acted) = self.act(&request, stop);
            let ms = started.elapsed().as_millis() as u64;
            let acted = Acted { ms, ..acted };
            // Finished before the line is printed: a reader that sees `stored: 2` and then
            // kills the process finds the row closed, not claimed by nobody.
            let _ = self.queue.finish(request.id, acted.error.as_deref());
            self.report
                .line(&line(&kind, Some(request.id), &acted));
            self.refresh_briefs(&kind, &acted, newest);
            if let Some(path) = self.status_path() {
                let summary = match &acted.error {
                    Some(err) => format!("{kind} failed: {err}"),
                    None if acted.stored > 0 => {
                        format!("{kind} done: stored {}", acted.stored)
                    }
                    None => format!("{kind} done"),
                };
                atlas_ingest::live::set_idle(&path, self.queued_count(), Some(&summary));
            }
            ran += 1;
        }
        ran
    }

    /// Run every monitor that is due at this instant, one report each.
    fn run_due(&self) -> usize {
        let due = match self.native.service.tick(false) {
            Ok(tick) => tick.due,
            // Same rule: stderr, once per pass, and the loop keeps running — the monitors
            // live in the archive, and an archive that will not answer is reported by the
            // next check's own log row as well.
            Err(error) => {
                eprintln!(
                    "atlas-monitor: the monitors could not be asked who is due: {}",
                    crate::error::scrub(&error.message())
                );
                return 0;
            }
        };
        let mut ran = 0usize;
        for monitor_id in due {
            let started = Instant::now();
            if let Some(path) = self.status_path() {
                let name = self.monitor_label(monitor_id);
                let _ = atlas_ingest::live::LiveStatus::start(
                    path,
                    "monitor_run",
                    &name,
                    "scheduled monitor check",
                    MAX_STEPS,
                    Some(self.queued_count()),
                );
            }
            let newest = crate::brief::newest_article(self.native.service.store().as_ref());
            let report = self.native.service.run(monitor_id, Reason::Scheduler);
            let acted = Acted {
                tool: report.research.tool.clone(),
                ms: started.elapsed().as_millis() as u64,
                stored: report.research.stored,
                alerts: report.new_alerts(),
                monitor: Some(monitor_id),
                error: report.error.clone(),
            };
            self.report
                .line(&line("monitor_run", None, &acted));
            self.refresh_briefs("monitor_run", &acted, newest);
            if let Some(path) = self.status_path() {
                let summary = match &acted.error {
                    Some(err) => format!("scheduled check failed: {err}"),
                    None => format!("scheduled check done: stored {}", acted.stored),
                };
                atlas_ingest::live::set_idle(&path, self.queued_count(), Some(&summary));
            }
            ran += 1;
        }
        ran
    }

    /// Human-readable `(topic, phase, max_steps)` for `data/agent-status.json`.
    fn describe_request_start(&self, request: &Request) -> (String, String, usize) {
        let payload = serde_json::from_str::<Value>(&request.payload).unwrap_or(Value::Null);
        match request.kind.as_str() {
            "research" => {
                let q = text_of(&payload, &["query", "topic"]).unwrap_or_default();
                (q, "starting research".to_string(), steps_of(&payload))
            }
            "monitor_run" => {
                let label = monitor_of(&payload)
                    .ok()
                    .map(|id| self.monitor_label(id))
                    .unwrap_or_default();
                (label, "researching topic".to_string(), MAX_STEPS)
            }
            "brief" => {
                let scope = payload
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("window");
                let key = payload.get("key").and_then(Value::as_str).unwrap_or("");
                let label = if key.is_empty() {
                    scope.to_string()
                } else {
                    format!("{scope}: {key}")
                };
                (label, "writing briefing".to_string(), 1)
            }
            "enrich" => (String::new(), "enriching articles".to_string(), 1),
            "settings_set" => (String::new(), "applying settings".to_string(), 1),
            other => (String::new(), format!("running {other}"), 1),
        }
    }

    fn monitor_label(&self, monitor_id: i64) -> String {
        self.native
            .service
            .get(monitor_id)
            .ok()
            .flatten()
            .map(|m| m.name)
            .unwrap_or_else(|| format!("monitor #{monitor_id}"))
    }

    /// Keep the reader's briefings current after an action that stored at least one story.
    ///
    /// Bounded by [`crate::brief::BRIEF_REFRESH`] model calls and ordered by
    /// [`crate::brief::refresh_after`] — window, busiest countries, then the run's own
    /// scope — and never a failure of the run: a briefing that could not be written is one
    /// line on stderr, because the action the reader asked for did happen and its own line
    /// already says so. A build without the briefing seam spends nothing and says nothing.
    fn refresh_briefs(&self, kind: &str, acted: &Acted, newest_before: i64) {
        // Only a pass that stored *stories* can make a briefing stale: `enrich` and
        // `brief` report a `stored` count of their own, and neither adds an article.
        if acted.stored == 0 || !matches!(kind, "research" | "monitor_run" | "topic_add") {
            return;
        }
        let own = self.own_scope(acted);
        let hours = self.native.service.window_hours();
        let visits = match crate::brief::refresh_run(
            self.native.service.store().as_ref(),
            self.native.briefs.as_ref(),
            hours,
            newest_before,
            own,
        ) {
            Ok(visits) => visits,
            Err(error) => {
                eprintln!(
                    "atlas-monitor: the briefings could not be planned: {}",
                    crate::error::scrub(&error.message())
                );
                return;
            }
        };
        for visit in visits {
            if let crate::brief::Visit::Failed { scope, key, error } = visit {
                eprintln!("atlas-monitor: the {scope} briefing {key:?} was not refreshed: {error}");
            }
        }
    }

    /// The saved scope a run belongs to: its topic when the monitor watches one (that is
    /// the page a reader opens from the topic list), else the monitor itself.
    ///
    /// A bare `research` request has none — it carries a question, not a saved row — so its
    /// refresh stops at the window and the countries of the new stories.
    fn own_scope(&self, acted: &Acted) -> Option<(String, String)> {
        let id = acted.monitor?;
        let topic = self
            .native
            .service
            .get(id)
            .ok()
            .flatten()
            .and_then(|monitor| monitor.topic_id);
        Some(match topic {
            Some(topic) => ("topic".to_string(), topic.to_string()),
            None => ("monitor".to_string(), id.to_string()),
        })
    }

    /// One request, dispatched on its kind.
    ///
    /// The kinds are `atlas_store::requests::KINDS` — a closed set the store refuses
    /// anything else for — and each arm is one call into the seam that owns it.
    ///
    /// `pub(crate)` so a test can hand one hand-built request to one arm: the queue
    /// only stores the kinds `atlas-store` lists, and a kind the store does not know yet
    /// (today: `brief`) is refused at the door rather than at this match.
    pub(crate) fn act(&self, request: &Request, stop: &crate::stop::Stop) -> (String, Acted) {
        let payload: Value = match serde_json::from_str(&request.payload) {
            Ok(Value::Object(map)) => Value::Object(map),
            _ => {
                return (
                    request.kind.clone(),
                    Acted {
                        error: Some(format!(
                            "a {} request needs one JSON object as its payload",
                            request.kind
                        )),
                        ..Acted::default()
                    },
                )
            }
        };
        match request.kind.as_str() {
            "research" => {
                let query = match text_of(&payload, &["query", "topic"]) {
                    Ok(query) => query,
                    Err(error) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(error),
                                ..Acted::default()
                            },
                        )
                    }
                };
                let steps = steps_of(&payload);
                let outcome = self.native.research.research(&query, steps);
                let acted = Acted {
                    tool: outcome.last_tool().map(str::to_string),
                    stored: outcome.stored.len(),
                    error: outcome.error.clone(),
                    ..Acted::default()
                };
                (request.kind.clone(), acted)
            }
            "monitor_run" => {
                let monitor_id = match monitor_of(&payload) {
                    Ok(id) => id,
                    Err(error) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(error),
                                ..Acted::default()
                            },
                        )
                    }
                };
                let report = self.native.service.run(monitor_id, Reason::Button);
                (
                    request.kind.clone(),
                    Acted {
                        tool: report.research.tool.clone(),
                        stored: report.research.stored,
                        alerts: report.new_alerts(),
                        monitor: Some(monitor_id),
                        error: report.error.clone(),
                        ..Acted::default()
                    },
                )
            }
            "topic_add" => {
                // The button says "monitor this topic": one row becomes a monitor (the
                // user's saved query) and one check, because a check *is* one research
                // pass — it drives the pass, judges what came back, raises the alerts and
                // records `last_run`, so the cadence starts from this click instead of
                // running the same query a second time a moment later.
                //
                // `add_once`, not `add`: this arm is reached again when a claim goes
                // stale (a child that died mid-click leaves its lease behind), and the
                // replay must not mint a second monitor for the same saved query. It
                // still runs the check — the service cannot tell a replay from a second
                // click, and a click means "look now".
                let name = text_of(&payload, &["name", "topic", "query"]).unwrap_or_default();
                let query = match text_of(&payload, &["query", "topic", "name"]) {
                    Ok(query) => query,
                    Err(error) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(error),
                                ..Acted::default()
                            },
                        )
                    }
                };
                let mut draft = match Draft::parse(
                    &name,
                    &query,
                    integer_of(&payload, "threshold"),
                    integer_of(&payload, "cadence_minutes"),
                    integer_of(&payload, "topic_id"),
                ) {
                    Ok(draft) => draft,
                    Err(error) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(error.message().to_string()),
                                ..Acted::default()
                            },
                        )
                    }
                };
                // Create/activate the matching row in `topics` first so the Topics
                // view shows it right away and `save_article`'s topic gate accepts
                // stories collected by the research pass below.
                let topic_id = match self.native.service.ensure_topic(&draft.name, &draft.query) {
                    Ok(id) => id,
                    Err(error) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(error.message().to_string()),
                                ..Acted::default()
                            },
                        )
                    }
                };
                if draft.topic_id.is_none() {
                    draft.topic_id = Some(topic_id);
                }
                let monitor_id = match self.native.service.add_once(&draft) {
                    Ok(id) => id,
                    Err(error) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(error.message().to_string()),
                                ..Acted::default()
                            },
                        )
                    }
                };
                let report = self.native.service.run(monitor_id, Reason::Button);
                (
                    request.kind.clone(),
                    Acted {
                        tool: report.research.tool.clone(),
                        stored: report.research.stored,
                        alerts: report.new_alerts(),
                        monitor: Some(monitor_id),
                        error: report.error.clone(),
                        ..Acted::default()
                    },
                )
            }
            // The reader's "was ist da so?": one prepared summary for one scope. The
            // payload is validated *before* a model call is paid for — six scope words, a
            // key that fits its column, hours inside the range, and a credential refused
            // with the queue's own sentence — and the pass itself is
            // `atlas_ingest::Pipeline::brief` behind [`crate::brief::Briefs`].
            "brief" => {
                let ask = match crate::brief::Ask::parse(&payload) {
                    Ok(ask) => ask,
                    Err(sentence) => {
                        return (
                            request.kind.clone(),
                            Acted {
                                error: Some(sentence),
                                ..Acted::default()
                            },
                        )
                    }
                };
                let answer = self.native.briefs.brief(&ask.scope, &ask.key, ask.hours);
                let written = answer.get("ok").and_then(Value::as_bool).unwrap_or(false);
                (
                    request.kind.clone(),
                    Acted {
                        stored: usize::from(written),
                        error: if written {
                            None
                        } else {
                            Some(
                                answer
                                    .get("error")
                                    .and_then(Value::as_str)
                                    .unwrap_or("the briefing pass did not answer")
                                    .to_string(),
                            )
                        },
                        ..Acted::default()
                    },
                )
            }
            "enrich" => {
                let limit = integer_of(&payload, "limit")
                    .filter(|limit| *limit > 0)
                    .map(|limit| limit as usize)
                    .unwrap_or(ENRICH_LIMIT);
                let outcome = self.native.enrich.backfill(limit);
                (
                    request.kind.clone(),
                    Acted {
                        stored: outcome.analysed,
                        error: outcome.error.clone(),
                        ..Acted::default()
                    },
                )
            }
            // The window may not write the archive, so a setting it changes is a row like
            // any other. One key, named by the search tool's own constant: the queue is a
            // click's worth of intent, not a general-purpose writer of the archive's
            // settings, and a request for any other key is refused by name.
            "settings_set" => {
                // `data/config.json` first: the model path and the five numbers the launcher's
                // argv is built from are validated and written by
                // [`crate::settings_set::apply`], one call into `atlas-config` — the same
                // `Config::save` the CLI uses (atomic, `0600`, a symlinked file refused), so
                // there is one writer of that file in the product. A request that names only
                // `search_engine` is not that arm's business and falls through to the archive
                // branch below.
                let root = crate::settings_set::root_of(self.native.service.store());
                if let Some(root) = root.as_deref() {
                    if let Some(acted) = crate::settings_set::apply_with_store(
                        &payload,
                        root,
                        Some(self.native.service.store()),
                    ) {
                        return (request.kind.clone(), acted);
                    }
                    // A model the reader already has: the window can paste a path or a link
                    // but can start nothing, so the tool that can name a model runs here,
                    // once, as the agent's child. Same door as the settings above — the
                    // `settings_set` row the Settings page writes — because it is the same
                    // kind of intent: change the file the launcher reads.
                    if let Some(acted) = crate::import_model::apply(&payload, root, stop) {
                        return (request.kind.clone(), acted);
                    }
                }
                // The shared envelope is `{"settings": {"<key>": <value>}, "why": "…"}`
                // (agreed across the window, the store and here). The older flat
                // `{"key", "value"}` is still read, so a row written by yesterday's window
                // is not a row this loop refuses.
                let shared = payload.get("settings");
                let key = shared
                    .and_then(|map| map.as_object())
                    .and_then(|map| map.keys().next().map(String::as_str))
                    .or_else(|| payload.get("key").and_then(Value::as_str))
                    .unwrap_or("")
                    .trim();
                let engine_key = atlas_ingest::search::META_KEY;
                let value = shared
                    .and_then(|map| map.get(key))
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("value").and_then(Value::as_str))
                    .unwrap_or("");
                let acted = if key != engine_key {
                    Acted {
                        error: Some(format!(
                            "this window may set \"{engine_key}\" through the queue, not {key:?}"
                        )),
                        ..Acted::default()
                    }
                } else if !is_http(value) {
                    // `Engine::new` asks the fetch gate, and the gate is about *hosts*: an
                    // `ftp://` template has a public host and got through (measured —
                    // `ftp://search.example.com/?q={query}` was stored). The engine is a
                    // place this app sends a request, so the scheme is checked here too.
                    Acted {
                        error: Some(
                            "that address cannot be a search engine: it has to start with \
                             http:// or https://"
                                .to_string(),
                        ),
                        ..Acted::default()
                    }
                } else {
                    match atlas_ingest::search::Engine::new(value) {
                        // The gate decides, here as in the search tool: a template this app
                        // may not open, or one with no `{query}`, never reaches the archive.
                        None => Acted {
                            error: Some(
                                "that address cannot be a search engine: it has to be an \
                                 http(s) URL on a public host with {query} in it"
                                    .to_string(),
                            ),
                            ..Acted::default()
                        },
                        Some(engine) => {
                            match self.native.service.store().set_meta(engine_key, engine.template()) {
                                Ok(_) => Acted {
                                    stored: 1,
                                    ..Acted::default()
                                },
                                Err(error) => Acted {
                                    error: Some(error.message().to_string()),
                                    ..Acted::default()
                                },
                            }
                        }
                    }
                };
                (request.kind.clone(), acted)
            }
            other => (
                other.to_string(),
                Acted {
                    error: Some(format!(
                        "no action for request kind {:?} (expected one of: {})",
                        crate::error::scrub(other),
                        atlas_store::requests::KINDS.join(", ")
                    )),
                    ..Acted::default()
                },
            ),
        }
    }
}

/// The one line an action prints.
///
/// Five keys always, in this order — `kind`, `id`, `tool`, `ms`, `stored` — and three
/// optional ones only when they say something: `monitor` (what the action was about),
/// `alerts` (raised by a check), `error` (why it failed). A reader that only knows the
/// five still reads every line.
fn line(kind: &str, id: Option<i64>, acted: &Acted) -> Value {
    let mut line = json!({
        "kind": kind,
        "id": id,
        "tool": acted.tool,
        "ms": acted.ms,
        "stored": acted.stored,
    });
    if let Some(monitor) = acted.monitor {
        line["monitor"] = json!(monitor);
    }
    if acted.alerts > 0 {
        line["alerts"] = json!(acted.alerts);
    }
    if let Some(error) = &acted.error {
        line["error"] = json!(crate::error::scrub(error));
    }
    line
}

/// The first non-empty string among `keys`.
///
/// Two spellings, and only where the product already has two: the pipeline and a monitor
/// row call it `query`, the button a person presses says `topic`. A payload with neither
/// is refused with a sentence that names both, so a writer cannot get this wrong silently.
fn text_of(payload: &Value, keys: &[&str]) -> Result<String, String> {
    for key in keys {
        if let Some(text) = payload.get(*key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return Ok(text.to_string());
            }
        }
    }
    let names: Vec<String> = keys.iter().map(|key| format!("\"{}\"", key)).collect();
    Err(format!(
        "this request needs a non-empty {} — the sentence to research, as the user typed it",
        names.join(" (or ")
    ))
}

/// Does this address use one of the two schemes this app speaks?
///
/// ASCII-only and case-insensitive, so `HTTPS://` is a page and `ftp://` is not.
fn is_http(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// An optional whole number field; anything that is not one is refused by the caller's
/// own parser (a threshold out of range is `Draft::parse`'s answer, not this file's).
fn integer_of(payload: &Value, key: &str) -> Option<i64> {
    payload.get(key).and_then(|value| match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    })
}

/// Which monitor a request is about: `monitor_id`, or `id` for a caller that has only one.
fn monitor_of(payload: &Value) -> Result<i64, String> {
    match integer_of(payload, "monitor_id").or_else(|| integer_of(payload, "id")) {
        Some(id) if id > 0 => Ok(id),
        Some(id) => Err(format!("a monitor id must be positive, not {}", id)),
        None => Err(
            "a monitor_run request needs \"monitor_id\": {\"monitor_id\": 3}".to_string(),
        ),
    }
}

/// The model turns a request may take, clamped the way the pipeline clamps.
fn steps_of(payload: &Value) -> usize {
    integer_of(payload, "max_steps")
        .filter(|steps| *steps > 0)
        .map(|steps| (steps as usize).clamp(1, 40))
        .unwrap_or(MAX_STEPS)
}

/// The queue a project owns: `data/requests.db`, beside the archive.
///
/// Spelled here because it is the one file this crate opens for writing, and because the
/// window writes the same name: two spellings of a queue is a queue nothing claims.
pub fn queue_path(paths: &atlas_config::Paths) -> std::path::PathBuf {
    paths.data_dir.join("requests.db")
}


// ---------------------------------------------------------------------------
// the binary's entry point
// ---------------------------------------------------------------------------

/// `atlas-monitor serve`: open the queue and the archive, install the signal handlers, and
/// loop until a stop arrives.
///
/// Fails (one `{"error": …}` line on stdout, exit 1) only when the *setup* fails — the
/// queue or the archive will not open, or the pipeline cannot be built. An action that
/// fails is not a failure of the agent: it is a line and a finished row.
pub fn main_entry() {
    match serve_main() {
        Ok(actions) => {
            // Not a JSON action line, and deliberately: this is the agent saying it is
            // leaving, and a reader of the stream can tell it from an action because it
            // has no `kind`. It goes to stderr so stdout stays one-object-per-action.
            eprintln!("atlas-monitor: stopped after {} action(s)", actions);
            std::process::exit(0);
        }
        Err(message) => {
            println!("{}", json!({ "error": message }));
            std::process::exit(1);
        }
    }
}

fn serve_main() -> Result<usize, String> {
    let paths = atlas_config::Paths::discover();
    let queue = Requests::open(queue_path(&paths)).map_err(|error| {
        format!(
            "the request queue at {} will not open: {}",
            queue_path(&paths).display(),
            error.message()
        )
    })?;
    let store = Arc::new(
        atlas_store::Store::open(&paths.db).map_err(|error| error.message().to_string())?,
    );
    // The archive's base schema before the pipeline opens it — idempotent, and the same
    // call `atlas-ingest`'s own binary makes.
    store.init().map_err(|error| error.message().to_string())?;
    let native = crate::wiring::native_with_research(
        store,
        &paths.root,
        Arc::new(crate::schedule::SystemClock::new()),
    )
    .map_err(|error| error.message().to_string())?;
    let clock = crate::schedule::SystemClock::new();
    let stop = crate::stop::watch();
    let report = Stdout;
    let serve = Serve::new(&queue, &native, &clock, &report);
    Ok(serve.run(&stop))
}
