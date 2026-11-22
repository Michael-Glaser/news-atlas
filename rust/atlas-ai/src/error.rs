//! One error type for everything the local model can do wrong.
//!
//! Three properties matter to every caller:
//!
//! * [`AiError::repairable`] — the model produced a *call* this crate refused
//!   (unknown tool, arguments that are not JSON, a missing required argument).
//!   That is not a failure, it is a turn of the conversation: the loop feeds the
//!   text back and the model tries again. Nothing malformed is ever executed.
//! * [`AiError::is_transient`] — a connection-level failure, the one class worth
//!   retrying exactly once (llama.cpp accepting a socket a moment before it is
//!   ready to answer). Everything else is retried zero times.
//! * [`AiError::is_context_overflow`] — the prompt did not fit. `ai.py` retried
//!   such a call with a smaller budget and the newest tool results protected;
//!   [`crate::agent`] does the same.
//!
//! No variant ever carries prompt text, article text or model output. `detail`
//! holds server diagnostics (a status line, an error message the server sent
//! about the *request*, at most [`DETAIL_CHARS`] characters) — never a body this
//! crate was asked to keep private.

use std::fmt;

/// How much of a server-supplied diagnostic string an [`AiError`] may carry.
/// `ai.py` used 160 for a model message and 120 for a fallback line; one cap is
/// enough and it is the larger of the two.
pub const DETAIL_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// The caller's configuration is not a loopback HTTP endpoint, or a required
    /// setting is missing. Refused before a socket exists.
    Config(String),
    /// Nothing is listening, or the listener closed on us. The only transient class.
    Unreachable(String),
    /// The wall-clock deadline for the whole request passed.
    Timeout(String),
    /// The server answered with a non-2xx status.
    Http { status: u16, detail: String },
    /// The server says the prompt does not fit its context window.
    ContextOverflow(String),
    /// The bytes were not the response that was asked for (not JSON, missing
    /// `choices`, an SSE frame that is not an object).
    BadResponse(String),
    /// HTTP framing was invalid (bad chunk size, header block over the cap).
    Protocol(String),
    /// The model stream carried an error frame.
    ModelError(String),
    /// A malformed tool call. Repairable: fed back to the model, never executed.
    InvalidCall(String),
    /// The model asked for the same call twice in a row. A loop, not a retry.
    Loop(String),
    /// The step or token budget is spent.
    Budget(String),
    /// The caller asked the loop to stop.
    Cancelled,
    /// A tool handler refused or failed. Reported to the model as data.
    Tool(String),
}

impl AiError {
    /// True when the model can fix this itself on the next turn.
    pub fn repairable(&self) -> bool {
        matches!(self, AiError::InvalidCall(_) | AiError::Tool(_))
    }

    /// True only for a connection-level failure: the class [`retry_once`] retries.
    ///
    /// [`retry_once`]: crate::retry::retry_once
    pub fn is_transient(&self) -> bool {
        matches!(self, AiError::Unreachable(_))
    }

    /// True when the prompt was too large for the model's context window.
    pub fn is_context_overflow(&self) -> bool {
        matches!(self, AiError::ContextOverflow(_))
    }

    /// A stable, loggable kind: no prompt text, no model output, one word.
    pub fn kind(&self) -> &'static str {
        match self {
            AiError::Config(_) => "config",
            AiError::Unreachable(_) => "unreachable",
            AiError::Timeout(_) => "timeout",
            AiError::Http { .. } => "http",
            AiError::ContextOverflow(_) => "context",
            AiError::BadResponse(_) => "bad-response",
            AiError::Protocol(_) => "protocol",
            AiError::ModelError(_) => "model-error",
            AiError::InvalidCall(_) => "invalid-call",
            AiError::Loop(_) => "loop",
            AiError::Budget(_) => "budget",
            AiError::Cancelled => "cancelled",
            AiError::Tool(_) => "tool",
        }
    }

    /// The human sentence `ai.py` produced: one readable line instead of a raw
    /// transport error. Wording kept where it existed there.
    pub fn human(&self) -> String {
        match self {
            AiError::Config(reason) => format!("The local model is misconfigured: {}", reason),
            AiError::Unreachable(_) => {
                // One sentence, and it names the thing to check. A second sentence
                // here would be stacked under the UI's own advice line, which
                // already carries the command that starts the server
                // (`atlas-ui::views::chat_tool::error_line`).
                "The local llama.cpp server is not answering, so there is no model to ask."
                    .to_string()
            }
            AiError::Timeout(_) => {
                "The local model did not finish in time. Try again, or ask for less in one turn."
                    .to_string()
            }
            AiError::Http { status, detail } => {
                let detail = if detail.is_empty() {
                    "no detail".to_string()
                } else {
                    clip(detail, DETAIL_CHARS)
                };
                format!(
                    "The local model rejected the request (HTTP {}): {}",
                    status, detail
                )
            }
            AiError::ContextOverflow(_) => {
                "The local model ran out of context: the pages were too long. Ask for fewer \
                 sources (one or two articles) and try again."
                    .to_string()
            }
            AiError::BadResponse(_) => {
                "The local model answered with something this app could not read.".to_string()
            }
            AiError::Protocol(_) => {
                "The local model connection broke mid-answer. Try again.".to_string()
            }
            AiError::ModelError(detail) => format!(
                "The local model reported an error: {}",
                clip(detail, DETAIL_CHARS)
            ),
            AiError::InvalidCall(reason) => format!("Malformed tool call: {}", reason),
            AiError::Loop(reason) => format!(
                "The model repeated a tool call it had already made: {}. The loop stopped offering \
                 tools and answered with the results it had.",
                reason
            ),
            AiError::Budget(reason) => format!("Step/token budget spent: {}", reason),
            AiError::Cancelled => "Stopped on request.".to_string(),
            AiError::Tool(reason) => format!("A tool failed: {}", reason),
        }
    }

    /// The wire form the CLI prints and the bridge reads. `ok:false` is the
    /// verdict; `repairable` tells the caller whether to feed it back to the model.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "kind": self.kind(),
            "message": self.human(),
            "repairable": self.repairable(),
        })
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.human())
    }
}

impl std::error::Error for AiError {}

/// `text` cut to at most `limit` characters, on a character boundary, with the
/// whitespace runs of a multi-line server message collapsed to single spaces.
pub fn clip(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(text.len().min(limit));
    let mut pending = false;
    let mut seen = 0usize;
    for ch in text.chars() {
        if seen == limit {
            break;
        }
        if ch.is_whitespace() {
            pending = !out.is_empty();
            continue;
        }
        if pending {
            out.push(' ');
            pending = false;
            seen += 1;
            if seen == limit {
                break;
            }
        }
        out.push(ch);
        seen += 1;
    }
    out.trim_end().to_string()
}

/// The words llama.cpp (and the servers that wrap it) use when the *prompt* is the
/// problem. Not a news-word list and not a classification: it reads the text of an
/// HTTP refusal, which is a protocol detail.
pub const CONTEXT_WORDS: [&str; 4] = ["context", "exceed", "too long", "too large"];

/// Classify a non-2xx answer: is this the "prompt too large" refusal?
///
/// `ai.py::_is_context_error` treated every 400 as a context error, because
/// llama.cpp answers 400 with a body when the message list does not fit; a tuned
/// prompt budget depends on that recovery, so the rule is kept, and 413 (a body the
/// server refused to read at all) gets the same recovery.
///
/// What changed, and why: the words used to be enough **on any status**. A 500
/// whose text happened to contain "context" therefore arrived as a context
/// overflow — which drops the status the reader needs, tells them the pages were
/// too long, and makes [`crate::agent`] retry the whole call with half the budget
/// while the server is broken. A server fault now keeps its status; only a 4xx can
/// be about the prompt, and within a 4xx the words decide for the statuses that do
/// not already mean it.
pub fn looks_like_context_overflow(status: u16, detail: &str) -> bool {
    if !(400..500).contains(&status) {
        return false;
    }
    matches!(status, 400 | 413) || says_context_overflow(detail)
}

/// Does this message say the prompt did not fit?
///
/// For the answers that carry no HTTP status at all: an SSE error frame
/// (`{"error": {"message": …}}` inside a 200 stream) and a `choices`-less error
/// object. There the words are the only evidence there is, which is exactly why
/// they are not enough on their own once a status *is* known.
pub fn says_context_overflow(detail: &str) -> bool {
    let low = detail.to_ascii_lowercase();
    CONTEXT_WORDS.iter().any(|word| low.contains(word))
}

/// The typed error for a non-2xx answer: one decision, used by every call site.
///
/// The status is never invented and never dropped: [`AiError::Http`] carries it, and
/// its [`AiError::human`] sentence reads "The local model rejected the request
/// (HTTP 500): …". Only the context refusal is reclassified, because the loop knows
/// how to recover from exactly that one.
pub fn status_error(status: u16, body: &str) -> AiError {
    let detail = error_detail(body);
    if looks_like_context_overflow(status, &detail) {
        return AiError::ContextOverflow(detail);
    }
    AiError::Http { status, detail }
}

/// The `error` message out of a JSON error body, the way `ai.py` read it
/// (`{"error": {"message": …}}` or `{"error": "…"}`), or the body collapsed to one
/// short line. Never longer than [`DETAIL_CHARS`].
pub fn error_detail(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = value.get("error") {
            if let Some(message) = error.get("message").and_then(|m| m.as_str()) {
                return clip(message, DETAIL_CHARS);
            }
            if let Some(message) = error.as_str() {
                return clip(message, DETAIL_CHARS);
            }
            return clip(&error.to_string(), DETAIL_CHARS);
        }
    }
    clip(body, DETAIL_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairable_is_only_a_bad_call() {
        assert!(AiError::InvalidCall("x".into()).repairable());
        assert!(AiError::Tool("x".into()).repairable());
        assert!(!AiError::Timeout("x".into()).repairable());
        assert!(!AiError::Loop("x".into()).repairable());
    }

    #[test]
    fn a_connection_failure_is_the_only_transient_class() {
        assert!(AiError::Unreachable("refused".into()).is_transient());
        for error in [
            AiError::Timeout("x".into()),
            AiError::Http { status: 500, detail: String::new() },
            AiError::Protocol("x".into()),
            AiError::BadResponse("x".into()),
            AiError::Config("x".into()),
            AiError::Cancelled,
        ] {
            assert!(!error.is_transient(), "{:?}", error);
        }
    }

    #[test]
    fn detail_is_extracted_and_capped() {
        assert_eq!(
            error_detail(r#"{"error":{"message":"the request exceeds the available context size"}}"#),
            "the request exceeds the available context size"
        );
        assert_eq!(error_detail(r#"{"error":"nope"}"#), "nope");
        let long = "x".repeat(1000);
        assert_eq!(error_detail(&long).chars().count(), DETAIL_CHARS);
        assert_eq!(error_detail("  spaced\n out "), "spaced out");
    }

    #[test]
    fn a_400_is_a_context_overflow() {
        assert!(looks_like_context_overflow(400, ""));
        // This line used to assert the opposite (`500, "context size exceeded"` is
        // an overflow). That was the defect: a 500 is the server failing, and
        // calling it an overflow drops the status, misleads the reader and makes the
        // loop retry with half the budget. A server fault keeps its status now.
        assert!(
            !looks_like_context_overflow(500, "context size exceeded"),
            "a server fault is not a budget problem"
        );
        assert!(!looks_like_context_overflow(500, "boom"));
        assert!(!looks_like_context_overflow(502, "context"));
        // A body the server refused to read gets the same recovery as a 400.
        assert!(looks_like_context_overflow(413, "payload too large"));
        // ... and a 4xx that names the prompt is one, at any status.
        assert!(looks_like_context_overflow(422, "the prompt exceeds the context"));
        assert!(!looks_like_context_overflow(422, "invalid tool_choice"));
        assert!(!looks_like_context_overflow(429, "slow down"));
    }

    #[test]
    fn a_500_becomes_an_error_that_carries_its_status() {
        let error = status_error(
            500,
            r#"{"error":{"message":"failed to decode the batch","type":"server_error"}}"#,
        );
        assert_eq!(error.kind(), "http");
        match &error {
            AiError::Http { status, detail } => {
                assert_eq!(*status, 500);
                assert_eq!(detail, "failed to decode the batch");
            }
            other => panic!("500 must stay an HTTP error, got {:?}", other),
        }
        let human = error.human();
        assert!(human.contains("HTTP 500"), "{}", human);
        assert!(human.contains("failed to decode the batch"), "{}", human);

        // A 500 that mentions the context is still the server failing.
        assert_eq!(status_error(500, "context shift failed").kind(), "http");
        // The one 4xx the loop can recover from stays recoverable.
        assert_eq!(
            status_error(400, "the request exceeds the available context size").kind(),
            "context"
        );
        // A refusal with a body that is not JSON keeps a readable detail.
        assert_eq!(status_error(503, "  no slot available \n").kind(), "http");
    }

    #[test]
    fn human_never_carries_a_prompt() {
        // A hostile server message is clipped and collapsed, never echoed whole.
        let error = AiError::Http {
            status: 500,
            detail: format!("line one\nline two {}", "y".repeat(500)),
        };
        let human = error.human();
        assert!(!human.contains('\n'), "{}", human);
        assert!(human.chars().count() < 260, "{}", human);
    }
}
