//! `run` — the record of one check, and the claim that stops two of them.
//!
//! Decisions: what a check reports ([`RunReport`]), that a refused check is still a
//! report (with a status and a reason, so the log has no gaps), and that only one
//! check per monitor may be in flight at a time ([`InFlight`]).
//!
//! It does not: perform the check. That is [`crate::service::MonitorService::run`],
//! which owns the sequence — claim, research, select, judge, alert, log.
//!
//! # No double-run
//!
//! A check of one monitor takes minutes (the model reads pages). Two of them would
//! judge the same articles twice and alert twice, and a second click is exactly how
//! that would happen. So the second caller gets `status: "skipped"` and the reason,
//! and the attempt is logged — a refusal is information, not silence.

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::alert::Alert;
use crate::research::ResearchOutcome;
use crate::spec::{Monitor, Reason};

/// How a check ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// It ran and everything it tried worked.
    Ok,
    /// It ran, judged, and something inside it failed (the reason is in `error`).
    Partial,
    /// It could not run at all (no store, no research, a failing statement).
    Error,
    /// It did not run: another check of the same monitor was in flight, or the
    /// caller's budget was spent.
    Skipped,
    /// The monitor is gone.
    NotFound,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Partial => "partial",
            RunStatus::Error => "error",
            RunStatus::Skipped => "skipped",
            RunStatus::NotFound => "not-found",
        }
    }
}

/// What the research pass reported, minus the briefing text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResearchSummary {
    pub ok: bool,
    pub error: Option<String>,
    pub stored: usize,
    pub steps: usize,
    pub tool_calls: usize,
    /// The last tool the model called in the pass, when it called one.
    ///
    /// The check log keeps counts, not tool names (a log line is read by a person, and
    /// the counts are what a check is judged by). This field exists for the agent's
    /// one-line-per-action record, which answers "what is the AI doing right now" — so it
    /// is carried, and not logged.
    pub tool: Option<String>,
}

impl ResearchSummary {
    pub fn of(outcome: &ResearchOutcome) -> ResearchSummary {
        ResearchSummary {
            ok: outcome.ok,
            error: outcome.error.clone(),
            stored: outcome.stored.len(),
            steps: outcome.steps,
            tool_calls: outcome.tool_calls,
            tool: outcome.last_tool().map(str::to_string),
        }
    }
}

/// One check, from claim to log row.
#[derive(Clone, Debug)]
pub struct RunReport {
    pub status: RunStatus,
    /// Why this check ran — the field the log keeps.
    pub reason: Reason,
    pub monitor_id: i64,
    pub monitor_name: String,
    pub query: String,
    pub threshold: i64,
    /// The window someone has to read this report against: everything fetched
    /// after `since` was looked at.
    pub since: String,
    pub started: String,
    pub finished: String,
    pub elapsed_ms: i64,
    pub window_hours: i64,
    pub research: ResearchSummary,
    pub candidates: usize,
    pub judged: usize,
    /// Articles judged below the threshold (remembered, never re-judged).
    pub below_threshold: usize,
    /// Articles the model said are not reporting (remembered, never alert).
    pub not_news: usize,
    /// Articles already alerted for this monitor.
    pub duplicates: usize,
    /// Articles left for the next check because the budget was reached.
    pub deferred: usize,
    /// Candidates the check refused to judge because there was no text to judge:
    /// a page stored with neither a summary nor a body. Nothing was read, so
    /// nothing can be called reporting.
    pub unread: usize,
    pub alerted: Vec<Alert>,
    /// Which scorers answered, in order of first use: `model`, `query-terms`.
    pub scorers: Vec<&'static str>,
    pub error: Option<String>,
    /// A short sentence about anything unusual (a deleted topic, no candidates, a
    /// refused research pass) — safe to log, never article text.
    pub note: String,
    /// The `monitor_runs` row, when one was written.
    pub run_id: Option<i64>,
}

impl RunReport {
    /// A report for a check that could not start.
    pub fn refused(monitor_id: i64, reason: Reason, at: &str, note: &str, status: RunStatus) -> RunReport {
        RunReport {
            status,
            reason,
            monitor_id,
            monitor_name: String::new(),
            query: String::new(),
            threshold: 0,
            since: at.to_string(),
            started: at.to_string(),
            finished: at.to_string(),
            elapsed_ms: 0,
            window_hours: 0,
            research: ResearchSummary::default(),
            candidates: 0,
            judged: 0,
            below_threshold: 0,
            not_news: 0,
            duplicates: 0,
            deferred: 0,
            unread: 0,
            alerted: Vec::new(),
            scorers: Vec::new(),
            // The refusal is the error; the note stays empty so the check log does
            // not say the same sentence twice (`service::log` writes both).
            error: Some(note.to_string()),
            note: String::new(),
            run_id: None,
        }
    }

    /// A report for a check that is about to run.
    pub fn starting(monitor: &Monitor, reason: Reason, started: &str, since: &str, window_hours: i64) -> RunReport {
        RunReport {
            status: RunStatus::Ok,
            reason,
            monitor_id: monitor.id,
            monitor_name: monitor.name.clone(),
            query: monitor.query.clone(),
            threshold: monitor.threshold,
            since: since.to_string(),
            started: started.to_string(),
            finished: started.to_string(),
            elapsed_ms: 0,
            window_hours,
            research: ResearchSummary::default(),
            candidates: 0,
            judged: 0,
            below_threshold: 0,
            not_news: 0,
            duplicates: 0,
            deferred: 0,
            unread: 0,
            alerted: Vec::new(),
            scorers: Vec::new(),
            error: None,
            note: String::new(),
            run_id: None,
        }
    }

    /// Alerts this check wrote.
    pub fn new_alerts(&self) -> usize {
        self.alerted.len()
    }

    /// Did anything come of it?
    pub fn ok(&self) -> bool {
        matches!(self.status, RunStatus::Ok | RunStatus::Partial)
    }

    /// Add one sentence to the note the check log keeps.
    ///
    /// Every part goes through [`crate::error::scrub`] on the way in, so the note
    /// stays one cleaned line of at most [`crate::error::MAX_ERROR_CHARS`] — `log`
    /// would truncate it anyway, and a sentence cut in the middle is worse than a
    /// shorter one. An empty part adds nothing, so a caller can offer a sentence
    /// that may not apply.
    pub fn note_line(&mut self, part: &str) {
        let part = part.trim();
        if part.is_empty() {
            return;
        }
        let joined = if self.note.trim().is_empty() {
            part.to_string()
        } else {
            format!("{}; {}", self.note.trim(), part)
        };
        self.note = crate::error::scrub(&joined);
    }

    /// Which scorer answered, as one word: `model`, `query-terms`, `mixed`, or
    /// `none` when nothing was scored.
    pub fn scorer(&self) -> &'static str {
        match self.scorers.len() {
            0 => "none",
            1 => self.scorers[0],
            _ => "mixed",
        }
    }

    /// The compact shape the tool result and the bridge read.
    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok(),
            "status": self.status.as_str(),
            "reason": self.reason.as_str(),
            "monitor": { "id": self.monitor_id, "name": self.monitor_name },
            "query": self.query,
            "threshold": self.threshold,
            "window_hours": self.window_hours,
            "since": self.since,
            "started": self.started,
            "finished": self.finished,
            "elapsed_ms": self.elapsed_ms,
            "research": {
                "ok": self.research.ok,
                "error": self.research.error,
                "stored": self.research.stored,
                "steps": self.research.steps,
                "tool_calls": self.research.tool_calls,
            },
            "candidates": self.candidates,
            "judged": self.judged,
            "below_threshold": self.below_threshold,
            "not_news": self.not_news,
            "duplicates": self.duplicates,
            "deferred": self.deferred,
            "unread": self.unread,
            "scorer": self.scorer(),
            "new_alerts": self.new_alerts(),
            "alerts": self.alerted.iter().map(Alert::to_json).collect::<Vec<_>>(),
            "error": self.error,
            "note": self.note,
            "run_id": self.run_id,
        })
    }
}

/// The set of monitors with a check in flight.
///
/// A claim is released when its [`Claim`] is dropped, so a panic inside a check
/// cannot leave a monitor stuck "running" for the life of the process.
#[derive(Debug, Default)]
pub struct InFlight {
    ids: Mutex<HashSet<i64>>,
}

impl InFlight {
    pub fn new() -> InFlight {
        InFlight {
            ids: Mutex::new(HashSet::new()),
        }
    }

    /// Try to claim `monitor_id`. `None` means a check of that monitor is already
    /// running and this one must not start.
    pub fn claim(&self, monitor_id: i64) -> Option<Claim<'_>> {
        let mut ids = match self.ids.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !ids.insert(monitor_id) {
            return None;
        }
        Some(Claim {
            owner: self,
            monitor_id,
        })
    }

    /// Is a check of this monitor running right now?
    pub fn is_running(&self, monitor_id: i64) -> bool {
        match self.ids.lock() {
            Ok(guard) => guard.contains(&monitor_id),
            Err(poisoned) => poisoned.into_inner().contains(&monitor_id),
        }
    }

    /// Every monitor with a check in flight.
    pub fn running(&self) -> Vec<i64> {
        let ids = match self.ids.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut out: Vec<i64> = ids.iter().copied().collect();
        out.sort_unstable();
        out
    }

    fn release(&self, monitor_id: i64) {
        let mut ids = match self.ids.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        ids.remove(&monitor_id);
    }
}

/// One monitor's claim. Dropping it lets the next check start.
pub struct Claim<'a> {
    owner: &'a InFlight,
    monitor_id: i64,
}

impl Claim<'_> {
    pub fn monitor_id(&self) -> i64 {
        self.monitor_id
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.owner.release(self.monitor_id);
    }
}

/// One line for a log: reason, status and counts. Never a title, never a query.
pub fn log_line(report: &RunReport) -> String {
    format!(
        "monitor {} ({}) {} in {} ms: {} judged, {} new, {} below, {} not news, {} duplicates",
        report.monitor_id,
        report.reason.as_str(),
        report.status.as_str(),
        report.elapsed_ms,
        report.judged,
        report.new_alerts(),
        report.below_threshold,
        report.not_news,
        report.duplicates
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_for_the_same_monitor_is_refused_until_the_first_drops() {
        let inflight = InFlight::new();
        let first = inflight.claim(7).expect("first claim");
        assert!(inflight.is_running(7));
        assert!(inflight.claim(7).is_none());
        assert_eq!(inflight.running(), vec![7]);
        assert_eq!(first.monitor_id(), 7);
        drop(first);
        assert!(!inflight.is_running(7));
        assert!(inflight.claim(7).is_some());
    }

    #[test]
    fn two_monitors_do_not_block_each_other() {
        let inflight = InFlight::new();
        let _one = inflight.claim(1).expect("one");
        let _two = inflight.claim(2).expect("two");
        assert_eq!(inflight.running(), vec![1, 2]);
    }

    #[test]
    fn a_refused_report_is_a_report() {
        let report = RunReport::refused(
            3,
            Reason::Scheduler,
            "2026-11-05T10:00:00Z",
            "a check for this monitor is already running",
            RunStatus::Skipped,
        );
        assert!(!report.ok());
        assert_eq!(report.status.as_str(), "skipped");
        assert_eq!(report.to_json()["reason"], "scheduler");
    }

    #[test]
    fn the_scorer_word_says_who_answered() {
        let mut report = RunReport::refused(1, Reason::Button, "x", "", RunStatus::Ok);
        assert_eq!(report.scorer(), "none");
        report.scorers.push("model");
        assert_eq!(report.scorer(), "model");
        report.scorers.push("query-terms");
        assert_eq!(report.scorer(), "mixed");
    }

    #[test]
    fn a_refused_report_says_its_reason_once() {
        // `service::log` writes the note and then the error, so a refusal that put
        // the same sentence in both logged "monitor not found; monitor not found".
        let report = RunReport::refused(
            3,
            Reason::Tool,
            "2026-11-05T10:00:00Z",
            "monitor not found",
            RunStatus::NotFound,
        );
        assert_eq!(report.error.as_deref(), Some("monitor not found"));
        assert!(report.note.is_empty(), "the note is not a second copy: {:?}", report.note);
        assert_eq!(report.to_json()["error"], "monitor not found");
    }
}
