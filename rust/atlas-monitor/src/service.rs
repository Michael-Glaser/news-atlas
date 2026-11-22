//! `service` — one check, start to finish, and everything around it.
//!
//! This is the file `monitor.py`'s `check`, `check_all`, `_run_check` and
//! `mark_seen`/`unseen_count`/`recent_alerts` became. The sequence for one check is
//! fixed and lives in [`MonitorService::run`]:
//!
//! 1. take the single-flight claim (a second check of one monitor is refused),
//! 2. read the monitor (gone → `not-found`, with a log row),
//! 3. remember the window start: the last check, or `window_hours` ago,
//! 4. drive one research pass for the query ([`crate::research`]),
//! 5. select what is new ([`crate::candidates`]),
//! 6. judge each article ([`crate::judge`]), remembering below-threshold and
//!    not-reporting verdicts so the next check does not pay for them again,
//! 7. raise the alerts that crossed ([`crate::alert`]),
//! 8. record `last_run`, the hit count and one check-log row, with the reason.
//!
//! Decisions: the order of the steps, what happens when a step fails (a report, not
//! an exception), and that `last_run` advances only when the pass actually
//! progressed — a failed research pass keeps the window open so the articles it was
//! meant to find are not skipped.
//!
//! It does not: search, fetch, read or store an article (that is `atlas-ingest`),
//! judge news-worthiness (the model), or hold a thread. [`MonitorService::tick`] is
//! the scheduler's whole footprint here — a caller has to ask it.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use atlas_store::Store;
use serde_json::{json, Value};

use crate::alert;
use crate::candidates;
use crate::error::Result;
use crate::judge::{Judge, Judgement};
use crate::research::Research;
use crate::run::{InFlight, ResearchSummary, RunReport, RunStatus};
use crate::schedule::{Clock, Scheduler, Tick};
use crate::spec::{Draft, Monitor, Reason};
use crate::wiring::Settings;
use crate::ALL_BUDGET_SEC;

/// How many of the model's "not reporting" reasons one check keeps in its note.
/// One is enough to show the shape of the answer; the whole list would push the
/// note past one readable line.
const NOT_NEWS_REASONS_KEPT: usize = 2;

/// Everything a check needs, in one place.
pub struct MonitorService {
    store: Arc<Store>,
    research: Arc<dyn Research>,
    judge: Arc<dyn Judge>,
    /// A second judge to score an article the model did not answer for. `None` by
    /// default: scoring by the query's own terms is a decision the caller makes out
    /// loud, not a quiet default. When nobody wired one, an unjudged article stays
    /// unjudged (measured: run `monitor_runs` id 1 stored `scorer=query-terms` for
    /// all 8 articles because a starved call returned an empty answer — the reader's
    /// alerts then carried word-count scores attributed to a model that never spoke).
    fallback: Option<Arc<dyn Judge>>,
    clock: Arc<dyn Clock>,
    settings: Settings,
    /// Sentences every check of this service carries, from its construction: a budget the
    /// reader set and the app could not obey (a window outside its range, a server running
    /// smaller than the config says). Not a failure — a fact the log keeps.
    notes: Vec<String>,
    scheduler: Mutex<Scheduler>,
    inflight: InFlight,
}

impl std::fmt::Debug for MonitorService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MonitorService")
            .field("settings", &self.settings)
            .field("inflight", &self.inflight.running())
            .finish()
    }
}

impl MonitorService {
    /// Build a service from explicit seams — what a test does, and what a caller
    /// with its own pipeline does.
    ///
    /// The fallback judge is **not** set here. Until [`MonitorService::with_fallback`]
    /// wires one, an article the model did not answer for is left unjudged and the
    /// check says so — the reader's alerts never carry a word-count score wearing the
    /// model's name.
    pub fn new(
        store: Arc<Store>,
        research: Arc<dyn Research>,
        judge: Arc<dyn Judge>,
        clock: Arc<dyn Clock>,
        settings: Settings,
    ) -> MonitorService {
        MonitorService {
            store,
            research,
            judge,
            fallback: None,
            clock,
            settings,
            notes: Vec::new(),
            scheduler: Mutex::new(Scheduler::default()),
            inflight: InFlight::new(),
        }
    }

    /// Add a sentence every check of this service carries in its note.
    ///
    /// The wiring uses it for the reader's budgets: a `model_context` the config had to clamp,
    /// or a model server that reports a smaller window than the config asks for. The check log
    /// is where a reader looks after a thin answer, so that is where the reason belongs.
    pub fn with_note(mut self, note: impl Into<String>) -> MonitorService {
        self.notes.push(note.into());
        self
    }

    /// Wire a fallback judge, explicitly. [`crate::judge::QueryTermsJudge`] is the one to pass when a
    /// caller really wants term scoring; it is never installed behind the caller's back.
    pub fn with_fallback(mut self, judge: Arc<dyn Judge>) -> MonitorService {
        self.fallback = Some(judge);
        self
    }

    /// Set the scheduler's on/off state (the default is on).
    pub fn with_scheduler(self, scheduler: Scheduler) -> MonitorService {
        if let Ok(mut held) = self.scheduler.lock() {
            *held = scheduler;
        }
        self
    }

    pub fn with_settings(mut self, settings: Settings) -> MonitorService {
        self.settings = settings;
        self
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Create the monitor tables (`monitor.py::init`).
    pub fn init(&self) -> Result<atlas_store::monitors::Applied> {
        Ok(self.store.init_monitors()?)
    }

    /// For how far back a never-checked monitor looks.
    pub fn window_hours(&self) -> i64 {
        match self.settings.window_hours {
            Some(hours) => hours.clamp(1, 24 * 30),
            None => self.store.window_hours().max(1),
        }
    }

    fn now_epoch(&self) -> i64 {
        self.clock.now_epoch()
    }

    fn now_iso(&self) -> String {
        atlas_store::clock::iso_from_epoch(self.now_epoch())
    }

    fn iso_ago(&self, hours: i64) -> String {
        atlas_store::clock::iso_from_epoch(self.now_epoch() - hours.max(0) * 3_600)
    }

    // -- monitors ----------------------------------------------------------

    pub fn list(&self) -> Result<Vec<Monitor>> {
        Ok(self
            .store
            .monitors()
            .list()?
            .iter()
            .map(Monitor::from_row)
            .collect())
    }

    pub fn get(&self, id: i64) -> Result<Option<Monitor>> {
        Ok(self.store.monitors().get(id)?.as_ref().map(Monitor::from_row))
    }

    /// Ensure an active topic row exists in `topics` so the Topics view shows it
    /// immediately and `save_article`'s topic gate accepts stories from the pass.
    pub fn ensure_topic(&self, name: &str, query: &str) -> Result<i64> {
        let raw_name = if name.trim().is_empty() {
            query.trim()
        } else {
            name.trim()
        };
        let topic_name: String = raw_name.chars().take(80).collect();
        let raw_keywords = if query.trim().is_empty() {
            raw_name
        } else {
            query.trim()
        };
        let keywords: String = raw_keywords.chars().take(800).collect();
        let now = self.now_iso();
        Ok(self
            .store
            .monitors()
            .ensure_topic(&topic_name, &keywords, &now)?)
    }

    /// Save a new monitor. It is active from the start and has never run.
    pub fn add(&self, draft: &Draft) -> Result<i64> {
        let now = self.now_iso();
        Ok(self.store.monitors().add(
            &draft.name,
            &draft.query,
            draft.threshold,
            draft.cadence_minutes,
            draft.topic_id,
            &now,
        )?)
    }

    /// The monitor the reader already has for this draft, if any.
    ///
    /// A `topic_add` request is a queue row, and a row whose lease expired is claimed
    /// again (`atlas_store::requests`) — so one click can be delivered twice. What
    /// "the same" means is [`Monitor::watches`]: the saved query the reader sees in
    /// the window, which is the only identity a replayed row carries.
    pub fn existing(&self, draft: &Draft) -> Result<Option<i64>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|monitor| monitor.watches(draft))
            .map(|monitor| monitor.id))
    }

    /// Add the draft's monitor, or hand back the one that already watches it.
    ///
    /// The caller still runs the check either way: a click means "look now", and the
    /// service cannot tell a second click from a replayed row. What it can promise is
    /// that a replay does not leave two identical monitors in the window, each
    /// raising its own alert stream for one person's click.
    pub fn add_once(&self, draft: &Draft) -> Result<i64> {
        match self.existing(draft)? {
            Some(id) => Ok(id),
            None => self.add(draft),
        }
    }

    pub fn set_active(&self, id: i64, active: bool) -> Result<usize> {
        Ok(self.store.monitors().set_active(id, active)?)
    }

    pub fn set_cadence(&self, id: i64, minutes: i64) -> Result<usize> {
        Ok(self.store.monitors().set_cadence(id, crate::spec::cadence_of(minutes))?)
    }

    /// Remove a monitor and everything that pointed at it.
    pub fn delete(&self, id: i64) -> Result<usize> {
        Ok(self.store.monitors().delete(id)?)
    }

    // -- alerts ------------------------------------------------------------

    /// Alerts of the last `hours`, newest first, with their entities decoded and a
    /// way in that still exists.
    ///
    /// Each row goes through [`crate::alert::row_json_for_reader`], which drops an
    /// `article_id` whose article the archive no longer holds (retention prunes it;
    /// the alert stays). This is the reader's side of the same rule
    /// [`crate::alert::raise`] enforces on the way in: an alert row is a button, so
    /// it has to point somewhere the reader can go.
    pub fn alerts(&self, hours: i64, limit: i64, monitor: Option<&str>) -> Result<Vec<Value>> {
        let since = self.iso_ago(hours.clamp(1, 24 * 30));
        let rows = self.store.monitors().recent_alerts(&since, monitor, limit)?;
        Ok(rows
            .iter()
            .map(|row| crate::alert::row_json_for_reader(&self.store, row))
            .collect())
    }

    pub fn mark_seen(&self, alert_id: i64, seen: bool) -> Result<usize> {
        Ok(self.store.monitors().mark_alert_seen(alert_id, seen)?)
    }

    pub fn unseen(&self) -> Result<i64> {
        Ok(self.store.monitors().unseen_count()?)
    }

    /// The check log, newest first.
    pub fn runs(&self, monitor_id: Option<i64>, limit: i64) -> Result<Vec<Value>> {
        Ok(self
            .store
            .monitors()
            .runs(monitor_id, limit)?
            .iter()
            .map(atlas_store::Row::as_json)
            .collect())
    }

    // -- the scheduler -----------------------------------------------------

    /// Monitors with a check in flight right now (for a status line and a test).
    pub fn running(&self) -> Vec<i64> {
        self.inflight.running()
    }

    pub fn scheduler(&self) -> Scheduler {
        match self.scheduler.lock() {
            Ok(held) => *held,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    pub fn set_scheduler_enabled(&self, enabled: bool) {
        if let Ok(mut held) = self.scheduler.lock() {
            held.set_enabled(enabled);
        }
    }

    /// What the scheduler would do at this instant, and optionally do it.
    ///
    /// `run` = `false` is the read-only question ("who is due?"); `true` starts one
    /// check per due monitor with [`Reason::Scheduler`], so every one of them is
    /// logged with that reason.
    pub fn tick(&self, run: bool) -> Result<Tick> {
        let now_epoch = self.now_epoch();
        let now = atlas_store::clock::iso_from_epoch(now_epoch);
        let scheduler = self.scheduler();
        if !scheduler.enabled() {
            return Ok(Tick {
                enabled: false,
                now,
                due: Vec::new(),
                ran: Vec::new(),
                note: "the scheduler is switched off; nothing runs on its own".to_string(),
            });
        }
        let monitors: Vec<Monitor> = self
            .store
            .monitors()
            .active()?
            .iter()
            .map(Monitor::from_row)
            .collect();
        let due = scheduler.due(&monitors, now_epoch);
        let due_ids: Vec<i64> = due.iter().map(|monitor| monitor.id).collect();
        let ran: Vec<RunReport> = if run {
            due.iter()
                .map(|monitor| self.run(monitor.id, Reason::Scheduler))
                .collect()
        } else {
            Vec::new()
        };
        let note = if due_ids.is_empty() {
            "no monitor is at its cadence".to_string()
        } else {
            format!("{} monitor(s) due", due_ids.len())
        };
        Ok(Tick {
            enabled: true,
            now,
            due: due_ids,
            ran,
            note,
        })
    }

    // -- the check ---------------------------------------------------------

    /// Run one explicit check for one monitor. Never panics, never raises: what
    /// happened is the returned report, and every attempt is logged.
    pub fn run(&self, monitor_id: i64, reason: Reason) -> RunReport {
        let started_epoch = self.now_epoch();
        let started = atlas_store::clock::iso_from_epoch(started_epoch);
        let Some(claim) = self.inflight.claim(monitor_id) else {
            let mut report = RunReport::refused(
                monitor_id,
                reason,
                &started,
                "a check for this monitor is already running",
                RunStatus::Skipped,
            );
            self.log(&mut report);
            return report;
        };
        let report = self.check(monitor_id, reason, started_epoch, &started);
        drop(claim);
        report
    }

    fn check(
        &self,
        monitor_id: i64,
        reason: Reason,
        started_epoch: i64,
        started: &str,
    ) -> RunReport {
        let window_hours = self.window_hours();
        let mut monitor = match self.store.monitors().get(monitor_id) {
            Ok(Some(row)) => Monitor::from_row(&row),
            Ok(None) => {
                let mut report = RunReport::refused(
                    monitor_id,
                    reason,
                    started,
                    "monitor not found",
                    RunStatus::NotFound,
                );
                self.log(&mut report);
                return report;
            }
            Err(error) => {
                let mut report = RunReport::refused(
                    monitor_id,
                    reason,
                    started,
                    &format!("the archive did not answer: {}", error),
                    RunStatus::Error,
                );
                self.log(&mut report);
                return report;
            }
        };
        // A monitor outlives its topic: the saved query still stands, and the check
        // says what happened to the topic instead of refusing to run.
        let topic_note = match monitor.topic_id {
            Some(topic_id) => match self.store.monitors().topic_name(topic_id) {
                Ok(None) => {
                    monitor.topic_id = None;
                    Some(format!(
                        "topic {} no longer exists; the saved query is monitored instead",
                        topic_id
                    ))
                }
                _ => None,
            },
            None => None,
        };
        let since = match monitor.last_run_epoch() {
            Some(_) => monitor.last_run.clone(),
            None => atlas_store::clock::iso_from_epoch(started_epoch - window_hours * 3_600),
        };
        let mut report = RunReport::starting(&monitor, reason, started, &since, window_hours);
        report.note = topic_note.unwrap_or_default();
        for note in &self.notes {
            report.note_line(note);
        }
        let clock_start = Instant::now();

        // 4. one research pass: the model searches, reads, classifies, stores.
        let outcome = self.research.research(&monitor.query, self.settings.research_steps);
        report.research = ResearchSummary::of(&outcome);

        // 5. what is new since the last check.
        let selected = match candidates::new_since(
            &self.store,
            &monitor,
            &since,
            &outcome.stored,
            self.settings.max_candidates,
            self.settings.max_judged,
        ) {
            Ok(selected) => selected,
            Err(error) => {
                return self.finish(report, RunStatus::Error, Some(error.message().to_string()), clock_start)
            }
        };
        report.candidates = selected.articles.len();
        report.duplicates = selected.skipped_alerted;
        report.deferred = selected.skipped_budget;
        report.unread = selected.skipped_unread;

        // 6 + 7. judge, remember, alert.
        let mut failure: Option<String> = None;
        // Why the model said a page is not reporting. The reason is the whole
        // answer — a rejection with no reason is not something the reader or the
        // model can check — so the first one is kept for the check log, where the
        // alternative is a bare count.
        let mut not_news_reasons: Vec<String> = Vec::new();
        let mut unasked = 0usize;
        for article in &selected.articles {
            let verdict = match self.judge.judge(&monitor, article) {
                Judgement::NotNews { reason } => {
                    // An explainer never alerts, and never costs a second ask.
                    let _ = self.store.monitors().mark_seen(monitor.id, &article.key());
                    report.not_news += 1;
                    let reason = crate::error::scrub(&reason);
                    if !reason.is_empty() && not_news_reasons.len() < NOT_NEWS_REASONS_KEPT {
                        not_news_reasons.push(reason);
                    }
                    continue;
                }
                Judgement::Scored(verdict) => verdict,
                Judgement::Unasked { reason } => {
                    // The model was asked and did not answer. That is a failure
                    // inside a check that still ran, so it is reported — status
                    // `partial` and the reason in the log — and not left to look
                    // like a clean pass.
                    //
                    // Nothing is scored in its place unless a caller wired a fallback.
                    // Substituting the query's own terms here was the quiet default
                    // that made a word filter decide relevance while the run row said
                    // `partial` and the alerts said nothing: the article is left for
                    // the reader, unmarked, and the note below names what happened.
                    unasked += 1;
                    if failure.is_none() {
                        failure = Some(crate::error::scrub(&reason));
                    }
                    match self.fallback.as_ref() {
                        Some(fallback) => match fallback.judge(&monitor, article) {
                            Judgement::Scored(verdict) => verdict,
                            _ => continue,
                        },
                        None => continue,
                    }
                }
            };
            report.judged += 1;
            if !report.scorers.contains(&verdict.scorer.as_str()) {
                report.scorers.push(verdict.scorer.as_str());
            }
            if !verdict.fired(monitor.threshold) {
                // Below the threshold: remembered, so the next check asks nothing.
                let _ = self.store.monitors().mark_seen(monitor.id, &article.key());
                report.below_threshold += 1;
                continue;
            }
            let at = self.now_iso();
            match alert::raise(&self.store, &monitor, article, &verdict, &at) {
                Ok(Some(alert)) => report.alerted.push(alert),
                Ok(None) => report.duplicates += 1,
                // The first failure is the one the log keeps: a later sentence
                // would replace the reason the check actually turned on.
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error.message().to_string());
                    }
                }
            }
        }
        if !not_news_reasons.is_empty() {
            // One sentence, the model's own words, capped by `scrub`: the reader
            // asking "why was this story never alerted?" gets the model's answer
            // instead of a number.
            let said = format!(
                "the model judged {} page(s) not reporting: {}",
                report.not_news,
                not_news_reasons.join("; ")
            );
            report.note_line(&crate::error::scrub(&said));
        }
        if unasked > 0 {
            // One line, in the reader's words, because the alternative is a run row
            // that says `partial` and does not say what became of the articles. The
            // scored-by-terms wording appears only when a caller wired a fallback and
            // it was the only thing that answered, so term scoring is never the quiet
            // default it used to be (run `monitor_runs` id 1).
            let said = if report.scorer() == "query-terms" {
                format!(
                    "the model did not judge {} article(s); those were scored by the monitor's own query terms, which is not the model",
                    unasked
                )
            } else {
                format!(
                    "the model did not judge {} article(s); nothing was scored for them and they stay in the archive unmarked",
                    unasked
                )
            };
            report.note_line(&crate::error::scrub(&said));
        }
        if !outcome.ok && outcome.error.is_some() {
            failure = failure.or_else(|| outcome.error.clone());
            // The pass did not progress: do not move the window, so the articles it
            // was meant to find are still looked at next time.
            let status = if report.new_alerts() > 0 {
                RunStatus::Partial
            } else {
                RunStatus::Error
            };
            return self.finish(report, status, failure, clock_start);
        }
        let status = if failure.is_some() {
            RunStatus::Partial
        } else {
            RunStatus::Ok
        };
        self.finish(report, status, failure, clock_start)
    }

    /// Close a report: timestamps, `last_run`, and the check-log row.
    ///
    /// `last_run` moves only for a check that progressed (`ok` or `partial`): a
    /// check whose research pass failed keeps the window open, so the articles it
    /// was meant to find are not skipped by the next one.
    fn finish(
        &self,
        mut report: RunReport,
        status: RunStatus,
        error: Option<String>,
        clock_start: Instant,
    ) -> RunReport {
        report.status = status;
        report.error = error;
        report.finished = self.now_iso();
        report.elapsed_ms = clock_start.elapsed().as_millis() as i64;
        if matches!(status, RunStatus::Ok | RunStatus::Partial) {
            let _ = self
                .store
                .monitors()
                .touch_run(report.monitor_id, &report.finished, report.new_alerts() as i64);
        }
        self.log(&mut report);
        report
    }

    /// Write the check-log row. A failure to log is reported in the note, never
    /// raised: the check itself already happened.
    fn log(&self, report: &mut RunReport) {
        let note = {
            let mut note = report.note.clone();
            if let Some(error) = &report.error {
                if !note.is_empty() {
                    note.push_str("; ");
                }
                note.push_str(error);
            }
            crate::error::scrub(&note)
        };
        let row = atlas_store::monitors::NewRun {
            monitor_id: report.monitor_id,
            reason: report.reason.as_str(),
            started: &report.started,
            finished: &report.finished,
            status: report.status.as_str(),
            new_alerts: report.new_alerts() as i64,
            judged: report.judged as i64,
            window_hours: report.window_hours,
            scorer: report.scorer(),
            elapsed_ms: report.elapsed_ms,
            note: &note,
        };
        match self.store.monitors().add_run(&row) {
            Ok(id) => report.run_id = Some(id),
            Err(error) => {
                if !report.note.contains("could not be logged") {
                    report.note = crate::error::scrub(&format!(
                        "{}; the check could not be logged: {}",
                        report.note, error
                    ));
                }
            }
        }
    }

    /// Check every active monitor, bounded by [`ALL_BUDGET_SEC`].
    ///
    /// `monitor.py::check_all`: a monitor that would start after the budget is spent
    /// is reported as skipped rather than silently dropped.
    pub fn run_all(&self, reason: Reason) -> Vec<RunReport> {
        let started = Instant::now();
        let monitors: Vec<Monitor> = match self.store.monitors().active() {
            Ok(rows) => rows.iter().map(Monitor::from_row).collect(),
            Err(_) => Vec::new(),
        };
        let mut reports = Vec::with_capacity(monitors.len());
        for monitor in monitors {
            if started.elapsed().as_secs() >= ALL_BUDGET_SEC {
                let mut report = RunReport::refused(
                    monitor.id,
                    reason,
                    &self.now_iso(),
                    "skipped, the check-all budget was spent",
                    RunStatus::Skipped,
                );
                self.log(&mut report);
                reports.push(report);
                continue;
            }
            reports.push(self.run(monitor.id, reason));
        }
        reports
    }

    /// A status line: counts, the scheduler's state, nothing page-shaped.
    pub fn status(&self) -> Value {
        let scheduler = self.scheduler();
        let monitors = self.list().unwrap_or_default();
        json!({
            "monitors": monitors.len(),
            "active": monitors.iter().filter(|monitor| monitor.active).count(),
            "scheduled": monitors
                .iter()
                .filter(|monitor| monitor.active && monitor.cadence_minutes > 0)
                .count(),
            "unseen": self.unseen().unwrap_or_default(),
            "scheduler": scheduler.enabled(),
            "running": self.inflight.running(),
            "window_hours": self.window_hours(),
            "max_judged": self.settings.max_judged,
            "research_steps": self.settings.research_steps,
        })
    }
}
