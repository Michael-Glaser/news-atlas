//! `spec` — what a monitor is: a saved query with a threshold and a cadence.
//!
//! Decisions: the shape of a monitor ([`Monitor`]), what the user may create
//! ([`Draft`]: a query that is not empty, a threshold between 0 and 10, a cadence
//! that is either "manual only" or at least [`MIN_CADENCE_MINUTES`]), whether a
//! monitor is due at an instant ([`Monitor::is_due`]), and the closed set of
//! reasons a check may have ([`Reason`]).
//!
//! It does not: run anything, touch the store, or know what a score means. The
//! decision "on or off" is a flag here; only the scheduler reads it.

use std::fmt;

use atlas_store::monitors::MonitorRow;

use crate::error::{Error, Result};
use crate::verdict::query_terms;

/// What a new monitor's threshold is when the caller says nothing
/// (`monitor.py::add_monitor(threshold=5)`).
pub const DEFAULT_THRESHOLD: i64 = 5;

/// A score is 0–10, so a threshold outside that range cannot be crossed.
pub const MIN_THRESHOLD: i64 = 0;
pub const MAX_THRESHOLD: i64 = 10;

/// Manual only: the scheduler never runs this monitor.
pub const MANUAL_CADENCE: i64 = 0;

/// Faster than this and the scheduler would spend the whole session inside the
/// model; slower than a week and the cadence is indistinguishable from manual.
pub const MIN_CADENCE_MINUTES: i64 = 5;
pub const MAX_CADENCE_MINUTES: i64 = 7 * 24 * 60;

/// The name column's width (`monitor.py` truncated at 120).
pub const MAX_NAME_CHARS: usize = 120;

/// The query column's width (`monitor.py` truncated at 500).
pub const MAX_QUERY_CHARS: usize = 500;

/// A monitor, as the user owns it and as the archive stores it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub query: String,
    /// The topic this monitor watches, when it watches one. A monitor outlives
    /// the topic: a deleted topic leaves the saved query behind, and a check says
    /// so rather than failing.
    pub topic_id: Option<i64>,
    pub threshold: i64,
    /// `0` means manual only — no cadence, no scheduler.
    pub cadence_minutes: i64,
    pub active: bool,
    pub created: String,
    pub last_run: String,
    pub hits: i64,
    pub alerts: i64,
    pub unseen: i64,
}

impl Monitor {
    /// Read one monitor row (`atlas_store::monitors::MonitorRow`).
    pub fn from_row(row: &MonitorRow) -> Monitor {
        Monitor {
            id: row.id,
            name: row.name.clone(),
            query: row.query.clone(),
            topic_id: row.topic_id,
            threshold: row.threshold.clamp(MIN_THRESHOLD, MAX_THRESHOLD),
            cadence_minutes: row.cadence_minutes.max(MANUAL_CADENCE),
            active: row.active,
            created: row.created.clone(),
            last_run: row.last_run.clone(),
            hits: row.hits,
            alerts: row.alerts,
            unseen: row.unseen,
        }
    }

    /// Seconds since the epoch of the last check, or `None` when it never ran.
    ///
    /// `atlas_store::parse_epoch` answers `0` for a timestamp it cannot read, and
    /// `0` is 1970 — treated as "never", because a check that never happened must
    /// not look like one that happened at the epoch.
    pub fn last_run_epoch(&self) -> Option<i64> {
        let trimmed = self.last_run.trim();
        if trimmed.is_empty() {
            return None;
        }
        match atlas_store::parse_epoch(trimmed) {
            0 => None,
            secs => Some(secs),
        }
    }

    /// Is this monitor due at `now` (seconds since the epoch)?
    ///
    /// Only an active monitor with a cadence can be due, and a monitor that has
    /// never run is due immediately — the scheduler's first tick is what turns a
    /// saved query into a monitored one.
    pub fn is_due(&self, now: i64) -> bool {
        if !self.active || self.cadence_minutes <= MANUAL_CADENCE {
            return false;
        }
        let interval = self.cadence_minutes.saturating_mul(60);
        match self.last_run_epoch() {
            None => true,
            Some(last) => now.saturating_sub(last) >= interval,
        }
    }

    /// The terms of the monitor's own query, deduped and lower-cased.
    ///
    /// These are the user's words, never a vocabulary of the code's: see
    /// [`query_terms`].
    pub fn terms(&self) -> Vec<String> {
        query_terms(&self.query)
    }

    /// What a person calls this monitor: its name, or its query when unnamed.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.query
        } else {
            &self.name
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "query": self.query,
            "topic_id": self.topic_id,
            "threshold": self.threshold,
            "cadence_minutes": self.cadence_minutes,
            "active": self.active,
            "created": self.created,
            "last_run": if self.last_run.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(self.last_run.clone()) },
            "hits": self.hits,
            "alerts": self.alerts,
            "unseen": self.unseen,
        })
    }

    /// Whether this monitor is the one `draft` describes.
    ///
    /// The saved query the reader sees in the window is the whole identity: a
    /// `topic_add` row replayed after its lease expired carries name and query and
    /// nothing else, so a monitor watching the same topic *is* the same monitor.
    pub fn watches(&self, draft: &Draft) -> bool {
        folded(&self.name) == folded(&draft.name) && folded(&self.query) == folded(&draft.query)
    }
}

/// A name or query as [`Monitor::watches`] compares it: edge whitespace dropped,
/// inner whitespace collapsed, case ignored. `"Chip  Rules"` and `"chip rules"` are
/// one saved query.
fn folded(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Why a check ran. Closed on purpose: the log has to be readable, and a made-up
/// reason is worse than no row at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The user pressed the button for this monitor.
    Button,
    /// The scheduler reached this monitor's cadence.
    Scheduler,
    /// The local model called the monitor tool.
    Tool,
    /// "Check every active monitor" (a button, or the boot-time sweep).
    CheckAll,
}

impl Reason {
    /// Every reason, in log order.
    pub const ALL: [Reason; 4] = [
        Reason::Button,
        Reason::Scheduler,
        Reason::Tool,
        Reason::CheckAll,
    ];

    /// The word the check log stores.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Button => "button",
            Reason::Scheduler => "scheduler",
            Reason::Tool => "tool",
            Reason::CheckAll => "check-all",
        }
    }

    /// Read a reason a caller sent. Unknown spellings are refused, not guessed.
    pub fn parse(raw: &str) -> Option<Reason> {
        let needle = raw.trim().to_ascii_lowercase();
        Reason::ALL
            .into_iter()
            .find(|reason| reason.as_str() == needle || reason.word() == needle)
    }

    /// The same word without the hyphen, for a JSON field spelled `check_all`.
    fn word(self) -> &'static str {
        match self {
            Reason::CheckAll => "check_all",
            other => other.as_str(),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for Reason {
    fn default() -> Reason {
        Reason::Button
    }
}

/// A monitor the user is asking for, before it has an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub query: String,
    pub threshold: i64,
    pub cadence_minutes: i64,
    pub topic_id: Option<i64>,
}

impl Draft {
    /// Validate and clean what a caller sent.
    ///
    /// The name falls back to the query (a monitor without a name is still a
    /// monitor), the threshold is clamped to 0–10, and the cadence is either
    /// manual (`0`) or inside [`MIN_CADENCE_MINUTES`]–[`MAX_CADENCE_MINUTES`].
    pub fn parse(
        name: &str,
        query: &str,
        threshold: Option<i64>,
        cadence_minutes: Option<i64>,
        topic_id: Option<i64>,
    ) -> Result<Draft> {
        let query = clip_clean(query, MAX_QUERY_CHARS);
        if query.is_empty() {
            return Err(Error::new("a monitor needs a query"));
        }
        let mut name = clip_clean(name, MAX_NAME_CHARS);
        if name.is_empty() {
            name = query.chars().take(60).collect();
        }
        Ok(Draft {
            name,
            query,
            threshold: threshold.unwrap_or(DEFAULT_THRESHOLD).clamp(MIN_THRESHOLD, MAX_THRESHOLD),
            cadence_minutes: cadence_of(cadence_minutes.unwrap_or(MANUAL_CADENCE)),
            topic_id: topic_id.filter(|id| *id > 0),
        })
    }

    /// The same, from one JSON object (`wire::add`).
    pub fn from_json(input: &serde_json::Value) -> Result<Draft> {
        let field = |key: &str| input.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
        Draft::parse(
            field("name"),
            field("query"),
            input.get("threshold").and_then(serde_json::Value::as_i64),
            input
                .get("cadence_minutes")
                .and_then(serde_json::Value::as_i64),
            input.get("topic_id").and_then(serde_json::Value::as_i64),
        )
    }
}

/// `0` stays manual; anything else is clamped into the scheduler's range.
pub fn cadence_of(raw: i64) -> i64 {
    if raw <= MANUAL_CADENCE {
        MANUAL_CADENCE
    } else {
        raw.clamp(MIN_CADENCE_MINUTES, MAX_CADENCE_MINUTES)
    }
}

/// Untrusted text, one line, capped — `atlas-guard`'s cleaning, not a copy of it.
///
/// [`crate::verdict::clean_field`] rather than `atlas_guard::cleantext::inline`
/// alone, because a control character (a newline or a tab included) is *dropped*
/// by the guard and a dropped newline welds two words together: a query pasted
/// over two lines was stored as "chipexport rules", and that welded string is
/// both the model's criteria and the fallback's terms. `clean_field` folds a
/// whitespace run to one space first, then hands the result to the guard.
pub fn clip_clean(text: &str, max: usize) -> String {
    crate::verdict::clean_field(text, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(cadence: i64, last_run: &str, active: bool) -> Monitor {
        Monitor {
            id: 1,
            name: "Chips".to_string(),
            query: "chip export".to_string(),
            topic_id: None,
            threshold: 6,
            cadence_minutes: cadence,
            active,
            created: "2026-11-05T09:00:00Z".to_string(),
            last_run: last_run.to_string(),
            hits: 0,
            alerts: 0,
            unseen: 0,
        }
    }

    #[test]
    fn a_monitor_without_a_cadence_is_never_due() {
        assert!(!monitor(MANUAL_CADENCE, "", true).is_due(1_800_000_000));
    }

    #[test]
    fn an_inactive_monitor_is_never_due() {
        assert!(!monitor(60, "", false).is_due(1_800_000_000));
    }

    #[test]
    fn a_monitor_that_never_ran_is_due_and_then_waits_its_cadence() {
        let now = atlas_store::parse_epoch("2026-11-05T12:00:00Z");
        assert!(monitor(60, "", true).is_due(now));
        assert!(!monitor(60, "2026-11-05T11:31:00Z", true).is_due(now));
        assert!(monitor(60, "2026-11-05T11:00:00Z", true).is_due(now));
    }

    #[test]
    fn a_draft_needs_a_query_and_clamps_what_it_was_given() {
        assert!(Draft::parse("", "   ", None, None, None).is_err());
        let draft = Draft::parse("", "chip export", Some(99), Some(1), Some(-3)).expect("draft");
        assert_eq!(draft.name, "chip export");
        assert_eq!(draft.threshold, MAX_THRESHOLD);
        assert_eq!(draft.cadence_minutes, MIN_CADENCE_MINUTES);
        assert_eq!(draft.topic_id, None);
    }

    #[test]
    fn a_query_pasted_over_two_lines_keeps_its_words_apart() {
        // A textarea sends the newline; `atlas_guard` drops a control character
        // without leaving a separator, so the cleaning has to fold it first.
        let draft = Draft::parse("Chips\nwatch", "chip\nexport\trules", Some(6), None, None)
            .expect("draft");
        assert_eq!(draft.query, "chip export rules");
        assert_eq!(draft.name, "Chips watch");
        assert_eq!(
            draft.query.split_whitespace().collect::<Vec<&str>>(),
            vec!["chip", "export", "rules"],
            "the words the user typed are still three words"
        );
    }

    #[test]
    fn a_reason_is_read_only_from_its_own_spellings() {
        assert_eq!(Reason::parse("scheduler"), Some(Reason::Scheduler));
        assert_eq!(Reason::parse("check_all"), Some(Reason::CheckAll));
        assert_eq!(Reason::parse("because"), None);
    }

    #[test]
    fn a_replayed_click_matches_the_monitor_the_first_delivery_saved() {
        let saved = monitor(120, "", true);
        let same = Draft::parse("  chips ", "chip   export", None, None, None).expect("draft");
        assert!(saved.watches(&same), "case and whitespace are not identity");
        let other_query = Draft::parse("Chips", "chip subsidies", None, None, None).expect("draft");
        assert!(!saved.watches(&other_query), "a different query is another monitor");
        let other_name = Draft::parse("Chip rules", "chip export", None, None, None).expect("draft");
        assert!(!saved.watches(&other_name), "a different name is another monitor");
    }

    #[test]
    fn a_timestamp_that_does_not_parse_reads_as_never_run() {
        assert_eq!(monitor(60, "not a date", true).last_run_epoch(), None);
        assert_eq!(monitor(60, "", true).last_run_epoch(), None);
        assert!(monitor(60, "2026-11-05T11:00:00Z", true)
            .last_run_epoch()
            .is_some());
    }
}
