//! atlas-ai — the local model, in Rust.
//!
//! This crate is the port of `server/ai.py` **minus everything that only existed
//! because Python was the host**: the subprocess launcher for `llama-server`
//! (a supervisor's job, not a library's), the `threading` bridges, the urllib
//! retry soup, the module-level caches with no owner, and the tool
//! implementations themselves (`db`, `graph`, `browser` are their own crates).
//!
//! What is left is the part that has to be exactly right:
//!
//! | Module | What it owns |
//! | --- | --- |
//! | [`http1`] | HTTP/1.1 to `127.0.0.1` only — the request, the response, the framing, the caps |
//! | [`endpoint`] | the caller's base URL, validated: loopback literal, a named port, no credentials |
//! | [`chunked`] | `Transfer-Encoding: chunked` out of arbitrary byte boundaries |
//! | [`deadline`] | one wall-clock budget per exchange or per turn, and which socket errors mean "the clock" |
//! | [`retry`] | one retry, only on a transient connect failure |
//! | [`sse`] | SSE frames out of arbitrary byte boundaries, with a truncated tail reported instead of executed |
//! | [`chat`] | the chat-completions request/response, streaming with tool-call fragment aggregation, the [`LlamaClient`] trait, and [`HttpLlamaClient`] |
//! | [`tool`] | the tool-call protocol: native and JSON-in-text parsing, schema validation, typed calls |
//! | [`agent`] | the loop: model → tool → model, events, step, token and wall-clock budgets, the same-call-twice brake, cancellation |
//! | [`context`] | the context-window rules: estimates, tool-result capping, trimming priority |
//! | [`prompts`] | the tuned prompt strings (verbatim) and the entity/category coercions |
//! | [`fence`] | the untrusted-data fence: how page text is wrapped, and how a page writing its own marker is disarmed |
//! | [`verdict`] | what the model answered about a page, and the six-hour cache of that answer |
//! | [`sampling`] | the per-job temperature / `max_tokens` table and the clamp every request goes through |
//! | [`health`] | is the model answering, which id, what context size |
//! | [`specs`] | the 23 ATLAS tool schemas (the registry is what decides which of them a run may call) |
//!
//! # The rules this crate keeps
//!
//! * **Loopback only.** [`endpoint::Endpoint::parse`] refuses anything else, and
//!   the socket is opened to a loopback literal — there is no name resolution in
//!   this crate at all.
//! * **Nothing is downloaded and nothing is logged.** No prompt, no article text
//!   and no model output leaves this crate as a log line; warnings are counts and
//!   reasons. The one exception is deliberate and visible: the answer is *returned*
//!   to the caller, which is what a caller asked for.
//! * **Model output is untrusted.** It is parsed, validated and capped before
//!   anything types it. A malformed call is a repairable error the loop feeds back,
//!   never a call that runs.
//! * **Prompts are data, not prose.** [`prompts::SYSTEM`] and the three tuned
//!   prompts are byte-for-byte the strings `ai.py` sent.
//! * No `unsafe`, no globals, no threads inside the library: the loop is
//!   synchronous and cancellable through its event callback, so the server decides
//!   how it is scheduled.
//!
//! # Using it
//!
//! ```no_run
//! use atlas_ai::{agent, chat::HttpLlamaClient, tool::ToolRegistry};
//! use std::time::Duration;
//!
//! let client = HttpLlamaClient::new("http://127.0.0.1:8080", "local", Duration::from_secs(600))?;
//! let registry = ToolRegistry::new();          // the caller's handlers go here
//! let config = agent::AgentConfig::default();  // prompts::SYSTEM, 10 steps, 24k tokens
//! let mut events = Vec::new();
//! let outcome = agent::run(
//!     &client,
//!     &registry,
//!     &config,
//!     &[],
//!     "What happened in the chip market today?",
//!     &mut |event| { events.push(event); atlas_ai::Control::Continue },
//! )?;
//! println!("{}", outcome.content);
//! # Ok::<(), atlas_ai::AiError>(())
//! ```
//!
//! # Wiring it under `server/ai.py`
//!
//! `server/aicore.py` is the bridge, in the shape of `server/textcore.py`: it runs
//! `rust/target/release/atlas-ai` with one subcommand and reads one JSON object
//! back. The call sites that can move today, one at a time, are the one-shot ones:
//!
//! | `ai.py` | becomes |
//! | --- | --- |
//! | `ai.server_ok()` | `aicore.server_ok()` |
//! | `ai.model_id()` | `aicore.model_id()` |
//! | `ai.chat_simple(prompt, t, n)` | `aicore.chat_simple(prompt, t, n)` (prose in on stdin) |
//! | `ai.news_verdict(...)` | `aicore.news_verdict(...)` |
//! | `ai.analyze_article(id)`'s model call | `aicore.analyze(title, summary, text, id)` |
//! | `ai.ensure_server()` | stays in Python: starting a child process is a supervisor's job |
//! | `ai.agent(...)` | stays in Python until the 23 tool handlers exist in Rust |
//!
//! `ai.agent` is the one thing that does not move yet, and that is deliberate: the
//! loop is here and tested, the *handlers* are not, and this crate executes no tool
//! it does not own.

#![forbid(unsafe_code)]

pub mod agent;
pub mod brief;
pub mod budget;
pub mod chat;
pub mod context;
pub mod error;
pub mod fence;
pub mod health;
pub mod judge;
pub mod prompts;
pub mod specs;
pub mod chunked;
pub mod deadline;
pub mod endpoint;
pub mod http1;
pub mod retry;
pub mod sampling;
pub mod sse;
pub mod tool;
pub mod verdict;
pub mod wire;

#[cfg(test)]
mod tests;

/// What an event callback says back to the loop.
///
/// Nothing else can stop a run: a model cannot ask to be cancelled, and a tool
/// cannot either. Only the caller that is streaming the events can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Keep going.
    Continue,
    /// Stop now. The run reports [`AiError::Cancelled`].
    Cancel,
}

impl Default for Control {
    fn default() -> Control {
        Control::Continue
    }
}

pub use agent::{
    calls_refused_sentence, event_json, no_tools_sentence, AgentConfig, Event, Outcome,
    RepeatDetector, StopReason, MAX_STEPS, OUT_OF_TOKENS, REPEAT_FALLBACK, REPEAT_NUDGE,
    REPEAT_WINDOW, TOTAL_TOKEN_BUDGET,
};
pub use chat::{ChatMessage, ChatReply, ChatRequest, HttpLlamaClient, LlamaClient, RawCall};
pub use budget::{Budgets, ANSWER_RANGE, CONTEXT_RANGE, REASONING_RANGE};
pub use context::{
    estimate_tokens, estimate_value_tokens, fit_context, fit_context_with_tools, model_json,
    prompt_room, prompt_room_in, truncate_for_ui, Fit, CONTEXT_TOKENS, PROMPT_TOKENS,
    TOOL_RESULT_CHARS,
};
pub use error::AiError;
pub use health::{ServerInfo, UNKNOWN_MODEL};
pub use prompts::{
    analysis_prompt, briefing_prompt, data_block, defang, news_prompt, NewsVerdict, VerdictCache,
    ANALYSIS_PROMPT, BRIEFING_PROMPT, NEWS_PROMPT, SYSTEM,
};
pub use sampling::{bounded as bounded_sampling, Job};
pub use specs::tools as tool_specs;
pub use sse::{SseEvent, SseParser};
pub use tool::{
    calls_in_text_counted, coerce_args, parse_json_loose, shown_name, turn_of, turn_of_counted,
    PendingCall, ToolCall, ToolHandler, ToolRegistry, ToolSpec, Turn, MAX_ARGUMENT_BYTES,
    MAX_CALLS_PER_TURN, MAX_NAME_SHOWN,
};
pub use chunked::ChunkedDecoder;
pub use deadline::Deadline;
pub use endpoint::Endpoint;
pub use http1::LoopbackHttp;

/// The version of this crate, for a status line.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The largest body this crate will read from the model in one answer.
pub const MAX_RESPONSE_BYTES: usize = http1::DEFAULT_MAX_BODY;

/// How long one model call may take, streaming included. `ai.py` used 600 s for a
/// completion and 10 s for `/v1/models`; the caller chooses, and this is the
/// completion number.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a health check may take — `ai.py`'s `_get(..., timeout=10)`.
pub const HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
