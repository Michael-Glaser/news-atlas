//! `research` — the seam a check drives, and the one production implementation.
//!
//! Decisions: what a research pass reports back ([`ResearchOutcome`]: did it work,
//! what did it store, how many turns did it take), and that the production
//! implementation is `atlas-ingest`'s own pipeline ([`IngestResearch`]) — the model
//! searches, reads, classifies and stores through its tools; this crate never
//! searches and never stores an article.
//!
//! It does not: build the pipeline (the caller does, from its config), call the
//! model directly, or interpret the briefing text. The answer is carried through
//! for the caller that asked for it and is never logged.
//!
//! [`NoResearch`] is the seam for a caller that only reads (the CLI listing
//! alerts): it reports that no pass happened, so nothing can be mistaken for a run.

use std::sync::Arc;

use serde_json::Value;

use atlas_ingest::Pipeline;

/// What one research pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResearchOutcome {
    pub ok: bool,
    /// Why it did not work, when it did not. A reason, never a page's text.
    pub error: Option<String>,
    /// Article ids the pass reported as stored. A check treats them as new even
    /// when they are older than the last check.
    pub stored: Vec<i64>,
    /// The model's briefing. Returned to the caller that asked; never logged.
    pub answer: String,
    /// Model turns taken, and how many tool calls they made.
    pub steps: usize,
    pub tool_calls: usize,
    /// The tools the model called, in the order it called them, as the pipeline names
    /// them (`web_search`, `fetch_page`, `save_article`, …).
    ///
    /// Carried because the agent prints one line per action and "what is the AI doing"
    /// is answered by the tool, not by a count of ticks. It is the pipeline's own
    /// vocabulary passed through — this crate adds no name to it and filters none out.
    pub tools: Vec<String>,
}

impl ResearchOutcome {
    /// The last tool the model called, when it called one: what the pass was doing when
    /// it ended, which is the useful half of the list for one log line.
    pub fn last_tool(&self) -> Option<&str> {
        self.tools.last().map(String::as_str)
    }

    /// A pass that did not run.
    pub fn refused(reason: &str) -> ResearchOutcome {
        ResearchOutcome {
            ok: false,
            error: Some(crate::error::scrub(reason)),
            ..ResearchOutcome::default()
        }
    }

    /// The compact shape a report carries — no briefing text.
    pub fn summary_json(&self) -> Value {
        serde_json::json!({
            "ok": self.ok,
            "error": self.error,
            "stored": self.stored.len(),
            "steps": self.steps,
            "tool_calls": self.tool_calls,
        })
    }
}

/// A pass for one query. `Send + Sync`: a check can run on a request thread.
pub trait Research: Send + Sync {
    /// One pass. Never panics, never returns a half-written result.
    fn research(&self, query: &str, max_steps: usize) -> ResearchOutcome;
}

/// The real thing: `atlas-ingest`'s `research`, which is the agent loop with the
/// 23 tool handlers wired to the store, `atlas-net`, the local browser and the
/// rule engines.
pub struct IngestResearch {
    pipeline: Arc<Pipeline>,
}

impl IngestResearch {
    pub fn new(pipeline: Arc<Pipeline>) -> IngestResearch {
        IngestResearch { pipeline }
    }

    /// The pipeline this seam drives, for a caller that needs its store or rules.
    pub fn pipeline(&self) -> &Arc<Pipeline> {
        &self.pipeline
    }
}

impl Research for IngestResearch {
    fn research(&self, query: &str, max_steps: usize) -> ResearchOutcome {
        let answer = self.pipeline.research(query, Some(max_steps));
        outcome_from_answer(&answer)
    }
}

/// Read the shape `Pipeline::research` returns.
///
/// Kept public because the bridge reads the same shape: one place decides which
/// field means "stored", and an id list is never invented from a title.
pub fn outcome_from_answer(answer: &Value) -> ResearchOutcome {
    let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let error = answer
        .get("error")
        .and_then(Value::as_str)
        .map(crate::error::scrub);
    let stored = answer
        .get("stored")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default();
    // The pipeline reports its tool calls as `steps: [{"tool": "web_search", …}]`. A
    // tool name is a name the model chose from its registry, so nothing is filtered
    // here; a step without one (a plain model turn) is skipped.
    let tools = answer
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| step.get("tool").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ResearchOutcome {
        ok,
        error,
        stored,
        tools,
        answer: answer
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        steps: answer
            .get("steps_taken")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize,
        tool_calls: answer
            .get("tool_calls")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize,
    }
}

/// No pass at all: for a caller that only reads.
pub struct NoResearch {
    reason: String,
}

impl NoResearch {
    pub fn new(reason: impl Into<String>) -> NoResearch {
        NoResearch {
            reason: reason.into(),
        }
    }
}

impl Default for NoResearch {
    fn default() -> NoResearch {
        NoResearch::new("this caller does not run research")
    }
}

impl Research for NoResearch {
    fn research(&self, _query: &str, _max_steps: usize) -> ResearchOutcome {
        ResearchOutcome::refused(&self.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_id_list_is_read_and_a_missing_one_is_not_invented() {
        let answer = serde_json::json!({
            "ok": true,
            "text": "briefing",
            "stored": [{"id": 12, "title": "a"}, {"title": "no id"}],
            "steps_taken": 4,
            "tool_calls": 9
        });
        let outcome = outcome_from_answer(&answer);
        assert!(outcome.ok);
        assert_eq!(outcome.stored, vec![12]);
        assert_eq!(outcome.steps, 4);
        assert_eq!(outcome.tool_calls, 9);
        assert_eq!(outcome.answer, "briefing");
    }

    #[test]
    fn the_tools_the_model_called_are_carried_in_order_and_the_last_one_is_named() {
        let answer = serde_json::json!({
            "ok": true,
            "steps": [
                {"tool": "web_search", "args": {}},
                {"tool": "fetch_page", "args": {}},
                {"tool": "save_article", "args": {}},
                {"args": {}},
            ],
        });
        let outcome = outcome_from_answer(&answer);
        assert_eq!(outcome.tools, vec!["web_search", "fetch_page", "save_article"]);
        assert_eq!(outcome.last_tool(), Some("save_article"));
        // A pass that called nothing names nothing; it does not invent a tool.
        assert_eq!(outcome_from_answer(&serde_json::json!({"ok": false})).last_tool(), None);
    }

    #[test]
    fn a_failed_pass_reports_a_scrubbed_reason() {
        let outcome = outcome_from_answer(&serde_json::json!({
            "ok": false,
            "error": "the local model is not reachable\nretry"
        }));
        assert!(!outcome.ok);
        assert!(!outcome.error.unwrap().contains('\n'));
        assert!(outcome.stored.is_empty());
    }

    #[test]
    fn nothing_wired_is_reported_as_nothing_ran() {
        let outcome = NoResearch::default().research("chips", 14);
        assert!(!outcome.ok);
        assert!(outcome.error.is_some());
    }

    #[test]
    fn the_report_shape_carries_no_briefing_text() {
        let outcome = ResearchOutcome {
            ok: true,
            answer: "an answer".to_string(),
            stored: vec![1, 2],
            ..ResearchOutcome::default()
        };
        let json = outcome.summary_json();
        assert_eq!(json["stored"], 2);
        assert!(json.get("text").is_none());
    }
}
