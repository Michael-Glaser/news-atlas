//! One step of the loop: ask the model, then run what it asked for.
//!
//! Decisions: the request is trimmed to fit the prompt budget *once* (the
//! estimate is returned with the reply, so the message list is serialised once and
//! not twice); a reply that is empty gets one nudge; a call that fails validation
//! is never executed and its error goes back to the model as data; a call whose
//! name is not in the registry is refused and the model is told which names exist;
//! a result that is too long for the context is capped before it is stored; and a
//! model call is abandoned the moment the run's deadline passes, on the first
//! fragment after it — the alternative is a ten-minute socket timeout per call while
//! the reader waits.
//!
//! Three shapes are worth stating because both the model and the UI read them:
//!
//! * a **rejected** call answers `{"error": …}` and never runs, and its event
//!   carries the arguments as an object (`{"raw": "<what the model typed>"}` when
//!   they are not JSON), so `atlas-web`'s `args` field keeps one type;
//! * a call that **already ran** in this run answers
//!   `{"repeated": true, "note": …, "result": <the result>}` — the result is
//!   included because the trimming pass may have removed the message it first
//!   arrived in, and a model told "use that result" with the result gone asks
//!   again;
//! * a call whose **handler failed** answers `{"error": …, "hint": …}`: the tool's
//!   own words, plus the one thing the model can do about it.
//!
//! It does not: decide whether to take another step, count the budgets, or stop on
//! a cancel — [`crate::agent::run`] owns the sequence and checks the control it
//! gets back. Nothing here logs.

use serde_json::{json, Value};

use crate::chat::{ChatMessage, ChatReply, ChatRequest, LlamaClient};
use crate::context::{fit_context_with_tools, prompt_room_in, truncate_for_ui};
use crate::deadline::Deadline;
use crate::Control;
use crate::error::AiError;
use crate::tool::{canonical_json, PendingCall, ToolRegistry};

use super::check;
use super::state::{AgentConfig, Event};

/// The one thing the model can do about a failure that is not its own fault.
///
/// The system prompt already tells it to state a failure in one line instead of
/// guessing; this names the temptation the loop actually sees — retrying the
/// identical call, which returns the identical failure.
const TOOL_FAILED_HINT: &str = "the tool failed - this is not a mistake in your arguments, so the \
same call would fail again. Say in one line what failed and answer with what you already have.";

/// The note a re-used result carries.
const REPEATED_NOTE: &str = "this is the same call you already made in this run - its result is \
below, and calling it again would return this same thing.";

/// Why a call that hit the run's deadline is reported as.
///
/// An [`AiError::Timeout`] rather than [`AiError::Cancelled`], because the two mean
/// different things to whatever is watching: a cancel is the reader leaving and
/// wants no frame at all, a deadline is the turn running out and is the one thing
/// `run` turns into [`super::StopReason::Deadline`].
const TURN_DEADLINE_HIT: &str = "the turn ran out of its wall-clock deadline mid-answer";

/// One model call that always fits the prompt budget.
///
/// Returns the reply and the message-list estimate the trimming pass already
/// computed, so the list is serialised once per call and not twice. The estimate
/// covers the messages only: the schemas are the same bytes on every call and are
/// charged to the context window inside
/// [`crate::context::fit_context_with_tools`].
#[allow(clippy::too_many_arguments)]
pub(super) fn ask(
    client: &dyn LlamaClient,
    config: &AgentConfig,
    messages: &mut Vec<ChatMessage>,
    tools: Option<&Value>,
    budget: usize,
    keep_recent: usize,
    max_tokens: u32,
    step: usize,
    deadline: Option<Deadline>,
    emit: &mut dyn FnMut(Event) -> Control,
) -> Result<(ChatReply, usize), AiError> {
    // `prompt + max_tokens <= context`, whatever the caller configured — and the context
    // is the reader's window, not a constant.
    let budget = prompt_room_in(config.context, budget, max_tokens);
    let fit = fit_context_with_tools(messages, tools, budget, keep_recent);
    let mut prompt_tokens = fit.tokens;
    let mut request = ChatRequest::new(client.model().to_string(), messages.clone());
    request.temperature = config.temperature;
    request.max_tokens = max_tokens;
    request.enable_thinking = config.enable_thinking;
    request.stream = true;
    if let Some(tools) = tools {
        request.tools = Some(tools.clone());
        request.tool_choice = Some(config.tool_choice.clone());
    }
    let cancelled = std::cell::Cell::new(false);
    let out_of_time = std::cell::Cell::new(false);
    let mut on_chunk = |is_thinking: bool, delta: &str| -> Control {
        // The clock is checked on every fragment, which is the only place a slow
        // model can be interrupted: `complete_stream` blocks until the server or the
        // socket's own ten-minute timeout says otherwise, and by then the reader has
        // waited long enough to have asked twice.
        if deadline.map_or(false, |deadline| deadline.remaining().is_none()) {
            out_of_time.set(true);
            return Control::Cancel;
        }
        let event = if is_thinking {
            Event::Thinking {
                step,
                delta: delta.to_string(),
            }
        } else {
            Event::Text {
                step,
                delta: delta.to_string(),
            }
        };
        match emit(event) {
            Control::Continue => Control::Continue,
            Control::Cancel => {
                cancelled.set(true);
                Control::Cancel
            }
        }
    };
    match client.complete_stream_with_thinking(&request, &mut on_chunk) {
        Ok(reply) => {
            // A client is free to finish the call anyway; the answer it built after
            // the deadline is not one this turn can use.
            if out_of_time.get() {
                return Err(AiError::Timeout(TURN_DEADLINE_HIT.to_string()));
            }
            Ok((reply, prompt_tokens))
        }
        Err(error) => {
            if out_of_time.get() {
                return Err(AiError::Timeout(TURN_DEADLINE_HIT.to_string()));
            }
            if cancelled.get() {
                return Err(AiError::Cancelled);
            }
            // Context overflow: drop the oldest tool payloads, keep the newest two,
            // and retry once with half the budget — `ai.py`'s recovery, unchanged.
            // The schemas are still offered, so the retry room is measured with
            // them: half of a budget that already paid for the schemas is a
            // prompt that is genuinely smaller.
            if error.is_context_overflow() {
                // `ai.py`'s `max(1200)` floor is kept, but never above the budget
                // that just failed: a retry that asks for *more* room than the
                // refused request is a retry that is guaranteed to be refused.
                let half = (budget / 2).max(1200).min(budget);
                let retry = fit_context_with_tools(messages, tools, half, 2);
                prompt_tokens = retry.tokens;
                let mut retry_request = request;
                retry_request.messages = messages.clone();
                return match client.complete_stream_with_thinking(&retry_request, &mut on_chunk) {
                    Ok(reply) => {
                        if out_of_time.get() {
                            return Err(AiError::Timeout(TURN_DEADLINE_HIT.to_string()));
                        }
                        Ok((reply, prompt_tokens))
                    }
                    Err(error) => {
                        // The retry is a call like any other: a cancel inside it is a
                        // cancel, a deadline is the clock, and a second failure is
                        // reported once, here, so the caller is never left with an
                        // error nobody was told about.
                        if out_of_time.get() {
                            return Err(AiError::Timeout(TURN_DEADLINE_HIT.to_string()));
                        }
                        if cancelled.get() {
                            return Err(AiError::Cancelled);
                        }
                        let _ = emit(Event::Error {
                            error: error.clone(),
                        });
                        Err(error)
                    }
                };
            }
            let _ = emit(Event::Error {
                error: error.clone(),
            });
            Err(error)
        }
    }
}

/// The arguments a `ToolResult` event shows when the call never became typed.
///
/// Always an object: `atlas-web` publishes `"args":{…}` and the UI reads fields off
/// it, so a rejected call sends `{"raw": "<the argument text>"}` rather than a bare
/// JSON string that a reader has to special-case.
fn untyped_args(arguments: &str) -> Value {
    match crate::tool::parse_json_loose(arguments) {
        Some(Value::Object(map)) => Value::Object(map),
        _ => json!({ "raw": arguments }),
    }
}

/// Prepare, run and report one call: `(name, result)`.
///
/// Never returns a *tool* error: a rejected or failing call is a result the model
/// can read. It does return [`AiError::Cancelled`] when the caller stops the run,
/// because honouring a cancel is not optional.
pub(super) fn execute_call(
    registry: &ToolRegistry,
    call: &PendingCall,
    seen: &mut Vec<(String, Value)>,
    step: usize,
    calls_made: &mut usize,
    emit: &mut dyn FnMut(Event) -> Control,
) -> Result<(String, Value), AiError> {
    let typed = match registry.prepare(call) {
        Ok(typed) => typed,
        Err(reason) => {
            // Malformed: never executed, always fed back.
            let result = json!({ "error": reason });
            check(emit(Event::ToolResult {
                step,
                // `name` and `args` are the model's own text here — it never became
                // a [`crate::tool::ToolCall`] — so both are capped on the way out:
                // shown_name for the name ([`crate::tool::MAX_NAME_SHOWN`], a 10 kB
                // name used to be a 10 907-byte frame) and `truncate_for_ui` for the
                // arguments (a 1 MB argument string used to be a 1 000 180-byte
                // frame). `atlas-web` publishes both straight to the browser.
                name: crate::tool::shown_name(call.name()),
                args: truncate_for_ui(&untyped_args(call.arguments())),
                result: truncate_for_ui(&result),
                error: true,
                repeated: false,
            }))?;
            return Ok((call.name().to_string(), result));
        }
    };
    *calls_made += 1;
    check(emit(Event::ToolCall {
        step,
        call: typed.clone(),
    }))?;

    // The key is the *validated* call, not the text: `{"limit":"5"}` and
    // `{"limit":5}` coerce to the same arguments and must not run twice.
    let key = format!(
        "{}\u{1}{}",
        typed.name,
        canonical_json(&Value::Object(typed.args.clone()))
    );
    if let Some((_, cached)) = seen.iter().find(|(existing, _)| *existing == key) {
        // The result travels with the note: the trimming pass may have replaced the
        // message it first arrived in, and "use that result" with no result is how
        // a run burns its budget asking the same question.
        let result = json!({
            "repeated": true,
            "note": REPEATED_NOTE,
            "result": cached,
        });
        check(emit(Event::ToolResult {
            step,
            name: typed.name.clone(),
            args: truncate_for_ui(&Value::Object(typed.args.clone())),
            result: truncate_for_ui(&result),
            error: false,
            repeated: true,
        }))?;
        return Ok((typed.name, result));
    }

    let result = match registry.call(&typed.name, &typed.args) {
        Ok(value) => value,
        Err(reason) => {
            // `ai.py::execute` prefixed a tool failure with the tool name; an
            // unknown tool already names itself.
            let message = if reason.starts_with("unknown tool") {
                reason
            } else {
                format!("{}: {}", typed.name, reason)
            };
            json!({ "error": message, "hint": TOOL_FAILED_HINT })
        }
    };
    let failed = result.get("error").is_some();
    check(emit(Event::ToolResult {
        step,
        name: typed.name.clone(),
        args: truncate_for_ui(&Value::Object(typed.args.clone())),
        result: truncate_for_ui(&result),
        error: failed,
        repeated: false,
    }))?;
    seen.push((key, result.clone()));
    Ok((typed.name, result))
}

/// A short, loggable description of what the model repeated.
///
/// "Already made" rather than "the previous step": the brake looks back over
/// [`super::state::REPEAT_WINDOW`] steps, and a sentence that names the wrong step
/// sends whoever reads the log looking in the wrong place.
pub(super) fn describe_calls(calls: &[PendingCall]) -> String {
    let mut names: Vec<&str> = calls.iter().map(PendingCall::name).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "{} call(s) this run had already made, with identical arguments ({})",
        calls.len(),
        names.join(", ")
    )
}

/// What the reply cost: the server's own count when it gives one, else four bytes
/// per token over the text and the tool-call arguments.
pub(super) fn reply_tokens(reply: &ChatReply) -> u64 {
    match reply.usage_tokens {
        Some(tokens) => tokens,
        None => {
            let bytes = reply.content.len()
                + reply
                    .calls
                    .iter()
                    .map(|call| call.arguments.len() + call.name.len())
                    .sum::<usize>();
            (bytes / 4) as u64
        }
    }
}
