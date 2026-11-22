//! The agent loop: model → tool → model, with a budget and a brake.
//!
//! This is `ai.py::agent`, with the Python host removed and three things made
//! explicit that Python left implicit:
//!
//! * **Tools are injected.** [`ToolRegistry`] holds the schemas and the handlers;
//!   this crate executes nothing. A handler that fails is data for the model, not
//!   a failure of the loop.
//! * **Every step is an event.** [`Event::Text`] for each streamed fragment,
//!   [`Event::ToolCall`] for a validated call, [`Event::ToolResult`] for what came
//!   back, then one [`Event::Done`] — or one [`Event::Error`]. A server can stream
//!   these to a browser without knowing anything about llama.cpp.
//! * **The loop cannot spin, and cannot leave the reader without an answer.**
//!   `ai.py` reused a cached result when the model repeated a call, which is a
//!   reasonable host behaviour but hides a model that is stuck. Here a step whose
//!   calls are exactly an earlier step's calls is reported
//!   ([`AiError::Loop`], on the event stream) and the loop stops offering tools
//!   ([`StopReason::RepeatedCall`]) — the run still ends in an answer, because a
//!   stuck model is a reason to stop asking questions, not a reason to withhold
//!   the one already written.
//!
//! Budgets, both enforced: `max_steps` (model turns) and `total_token_budget`
//! (estimated prompt + reply tokens). When either is spent the loop makes **one**
//! final call without tools, so the reader still gets an answer — the behaviour
//! `ai.py` had, kept because it is the difference between "no answer" and "an
//! answer that names what is missing". The sentence that call reads, and the one
//! the reader gets if it comes back empty, are [`StopReason::guidance`]'s and
//! [`StopReason::fallback`]'s: one per reason, so nothing reports the wrong
//! budget.
//!
//! A fourth bound is not a count at all: **a turn has a wall clock**
//! ([`TURN_DEADLINE`] unless the caller says otherwise). Steps and tokens count work,
//! and work is not time — one call has `crate::DEFAULT_TIMEOUT` of its own and a run
//! is up to eleven of them, so a slow local model could hold the reader for the better
//! part of two hours. The clock is checked between steps *and on every streamed
//! fragment*, which is the only place a call in progress can be interrupted. A run cut
//! this way keeps what the model had written, appends [`OUT_OF_TIME`], and makes no
//! rescue call — the rescue is exactly what there is no time for
//! ([`StopReason::Deadline`]).
//!
//! A third budget is not a number of turns or tokens but a number of *calls*: one
//! turn may ask for at most [`crate::tool::MAX_CALLS_PER_TURN`]. Steps do not bound
//! that — ten steps of a thousand calls is ten thousand handler calls — and before
//! the cap one 51 891-byte answer ran 1 000 handlers inside a single step. A turn
//! that asks for more gets the first calls and one sentence
//! ([`calls_refused_sentence`]), so the model can continue one or two at a
//! time and the run still ends in an answer.
//!
//! Verbatim from `ai.py`: the empty-answer nudge, the budget-spent sentence, the
//! two fallback sentences, and the `{"repeated": …}` / `{"error": …}` shapes the
//! model was tuned on.
//!
//! # The three files
//!
//! * this one — [`run`]: the sequence of steps, the budgets, the cancel, and the
//!   one final call when a budget is spent;
//! * `state` — what a run carries: [`AgentConfig`], [`Event`], [`StopReason`],
//!   [`Outcome`], [`RepeatDetector`], the two budget constants and the sentences a
//!   stopped run says;
//! * `step` — one step's halves: ask the model, then validate and execute the
//!   calls it made (or feed a malformed call back).

use serde_json::Value;

use crate::chat::{ChatMessage, LlamaClient};
use crate::Control;
use crate::error::AiError;
use crate::tool::{turn_of_counted, PendingCall, ToolRegistry, Turn};
use crate::prompts;
use crate::context::{model_json, TOOL_RESULT_CHARS};

mod state;
mod step;

#[cfg(test)]
mod tests;

pub use state::{
    calls_refused_sentence, event_json, no_tools_sentence, AgentConfig, Event, Outcome,
    RepeatDetector, StopReason, MAX_STEPS, OUT_OF_TIME, OUT_OF_TOKENS, REPEAT_FALLBACK,
    RESCUE_ROUNDS,
    REPEAT_NUDGE, REPEAT_WINDOW, TOTAL_TOKEN_BUDGET, TURN_DEADLINE,
};

use crate::deadline::Deadline;
use step::{ask, describe_calls, execute_call, reply_tokens};

/// Run the loop.
///
/// `history` is the reader's earlier turns (filtered by [`crate::chat::history`]);
/// `user` is the message being answered. `emit` is called for every event and may
/// stop the run by returning [`Control::Cancel`], which is reported as
/// [`AiError::Cancelled`] — the loop never continues past a cancel.
///
/// The run's wall clock is [`AgentConfig::deadline`] ([`TURN_DEADLINE`] unless the
/// caller says otherwise). A caller that already holds a deadline — a job that must
/// finish by a time, two runs sharing one budget — asks [`run_until`] instead and
/// keeps this call site unchanged.
pub fn run(
    client: &dyn LlamaClient,
    registry: &ToolRegistry,
    config: &AgentConfig,
    history: &[ChatMessage],
    user: &str,
    emit: &mut dyn FnMut(Event) -> Control,
) -> Result<Outcome, AiError> {
    run_until(
        client,
        registry,
        config,
        history,
        user,
        emit,
        config.deadline.map(Deadline::new),
    )
}

/// [`run`], with the turn's deadline decided by the caller.
///
/// `deadline` overrides [`AgentConfig::deadline`]; `None` means no clock at all.
/// The deadline is checked before every model call and inside each one, on every
/// streamed fragment, so a model that never stops writing is cut where it stands
/// rather than after its own ten-minute socket timeout. A run cut this way keeps
/// what the model had written and appends [`OUT_OF_TIME`]; there is no rescue call,
/// because the rescue is exactly what there is no time for.
pub fn run_until(
    client: &dyn LlamaClient,
    registry: &ToolRegistry,
    config: &AgentConfig,
    history: &[ChatMessage],
    user: &str,
    emit: &mut dyn FnMut(Event) -> Control,
    deadline: Option<Deadline>,
) -> Result<Outcome, AiError> {
    let mut messages: Vec<ChatMessage> = Vec::with_capacity(history.len() + 2);
    messages.push(ChatMessage::system(config.system.clone()));
    messages.extend(crate::chat::history(history));
    messages.push(ChatMessage::user(user));

    let tools = registry.wire();
    let with_tools = !registry.is_empty();
    let mut steps = 0usize;
    let mut calls_made = 0usize;
    let mut tokens: u64 = 0;
    let mut detector = RepeatDetector::new();
    let mut nudged = false;
    // Results already produced in this run, by call key, so a call the model
    // repeats later gets the answer it already has instead of the work twice.
    let mut seen: Vec<(String, Value)> = Vec::new();
    let mut stop: Option<StopReason> = None;
    // What the model has streamed so far. The caller already has it — it was framed
    // as it arrived — but a run cut by its deadline has no reply to return, and the
    // reader would otherwise lose the answer they watched being written.
    let streamed = std::cell::RefCell::new(String::new());
    let mut tracked = |event: Event| -> Control {
        if let Event::Text { delta, .. } = &event {
            streamed.borrow_mut().push_str(delta);
        }
        emit(event)
    };

    for step in 1..=config.max_steps.max(1) {
        if spent(deadline) {
            // Between two steps the clock is the only thing that can have run out:
            // asking again would be asking for more time than there is.
            stop = Some(StopReason::Deadline);
            break;
        }
        steps = step;
        // The last tool turn is told that it is the last tool turn.
        //
        // Measured on the shipped model, a research pass spent its eight steps on
        // `find_news`, one `web_search` and four `web_open`s and never reached
        // `save_article`: it read four pages and stored nothing, because nothing told it
        // that the budget — not the app — was what ended the run. One sentence, once, on
        // the turn where a saving call is still possible, is the difference between a run
        // that files what it read and one that explains why it could not.
        if with_tools && step == config.max_steps.max(1) {
            messages.push(ChatMessage::user(prompts::LAST_TOOL_TURN));
        }
        let (reply, prompt_tokens) = match ask(
            client,
            config,
            &mut messages,
            if with_tools { Some(&tools) } else { None },
            config.prompt_budget,
            config.keep_recent,
            config.max_tokens,
            step,
            deadline,
            &mut tracked,
        ) {
            Ok(pair) => pair,
            Err(error) => {
                if spent(deadline) {
                    stop = Some(StopReason::Deadline);
                    break;
                }
                return Err(error);
            }
        };
        tokens += prompt_tokens as u64 + reply_tokens(&reply);

        // `refused` is how many calls the turn's cap would not let through: the
        // number is one sentence on the way back to the model, and it is read here
        // rather than inside the parser because *this* is where the work happens.
        let (turn, refused) = turn_of_counted(&reply);
        match turn {
            Turn::Answer(text) => {
                detector.reset();
                let content = text.trim().to_string();
                if content.is_empty() {
                    if nudged {
                        return finish(
                            &mut tracked,
                            Outcome {
                                content: prompts::EMPTY_ANSWER.to_string(),
                                steps,
                                tool_calls: calls_made,
                                tokens,
                                stopped: StopReason::EmptyAnswer,
                            },
                        );
                    }
                    nudged = true;
                    messages.push(ChatMessage::user(prompts::NUDGE));
                    continue;
                }
                return finish(
                    &mut tracked,
                    Outcome {
                        content: with_cut_off(content, reply.truncated),
                        steps,
                        tool_calls: calls_made,
                        tokens,
                        stopped: StopReason::Answered,
                    },
                );
            }
            Turn::Calls(pending) => {
                if !with_tools {
                    // A model calling tools it was not offered: not fatal, and not
                    // silently ignored either — the loop says which call it did not
                    // make and asks for the answer. (A grammar can force a call even
                    // when `tools` was never sent.)
                    let name = pending
                        .first()
                        .map(|call| call.name().to_string())
                        .unwrap_or_default();
                    messages.push(ChatMessage::user(state::no_tools_sentence(&name)));
                    continue;
                }
                let keys: Vec<String> = pending.iter().map(PendingCall::key).collect();
                if detector.observe(keys) {
                    // A stuck model is a reason to stop *offering tools*, not a
                    // reason to withhold the answer: the reader gets the final
                    // no-tool call below, exactly as for a spent budget. The event
                    // says what happened; before this the run returned
                    // `AiError::Loop` and the reader got nothing but a red line.
                    check(tracked(Event::Error {
                        error: AiError::Loop(describe_calls(&pending)),
                    }))?;
                    stop = Some(StopReason::RepeatedCall);
                    break;
                }
                let text = reply.content.trim().to_string();
                let wire: Vec<Value> = pending.iter().map(PendingCall::to_wire).collect();
                messages.push(ChatMessage::assistant_calls(text, Value::Array(wire)));
                for call in pending.iter() {
                    let call_id = call.id.clone();
                    let (name, result) = execute_call(
                        registry,
                        call,
                        &mut seen,
                        step,
                        &mut calls_made,
                        &mut tracked,
                    )?;
                    messages.push(ChatMessage::tool(
                        call_id,
                        name,
                        model_json(&result, TOOL_RESULT_CHARS),
                    ));
                }
                // A turn that asked for more than [`MAX_CALLS_PER_TURN`] calls got
                // the first ones only. The model is told in one sentence — the
                // alternative, running all of them, is a thousand writes from one
                // answer — and the next step is an ordinary step with its own
                // budgets, so nothing here can spin.
                if refused > 0 {
                    messages.push(ChatMessage::user(state::calls_refused_sentence(refused)));
                }
            }
        }

        if tokens >= config.total_token_budget {
            stop = Some(StopReason::TokensSpent);
            break;
        }
    }

    // Budget spent, clock spent or model stuck: one last call without tools, so the
    // reader still gets an answer — except when the clock is what ran out, because
    // then there is no time for it. The answer is what the model had already
    // streamed, and the reason's sentence says why it stops there.
    let stopped = stop.unwrap_or(StopReason::StepsSpent);
    let content = if spent(deadline) {
        let spoken = streamed.borrow().trim().to_string();
        if spoken.is_empty() {
            stopped.fallback().to_string()
        } else {
            format!("{}\n\n{}", spoken, stopped.fallback())
        }
    } else {
        messages.push(ChatMessage::user(stopped.guidance()));
        // The final call is made without tools — and a model that still wanted to
        // call one writes the call as text. Measured twice on the shipped model: a
        // research pass that had read four articles ended with a
        // `<function=save_article>` block, so the reader got a tool call instead of an
        // answer and the archive got no row. A run is therefore allowed to *run* what
        // it wrote, for at most [`RESCUE_ROUNDS`] rounds, before the answer is taken:
        // the work the model asked for happens, and the next call is asked for prose.
        let mut answer: Option<String> = None;
        let mut rescued = 0usize;
        loop {
            steps += 1;
            let (reply, final_prompt_tokens) = match ask(
                client,
                config,
                &mut messages,
                None,
                config.prompt_budget,
                config.keep_recent,
                config.final_max_tokens,
                steps,
                deadline,
                &mut tracked,
            ) {
                Ok(pair) => pair,
                Err(AiError::Cancelled) => return Err(AiError::Cancelled),
                // A failed final call leaves no text to show; say what the reader can
                // act on.
                Err(_) => break,
            };
            tokens += final_prompt_tokens as u64 + reply_tokens(&reply);
            let (turn, _refused) = turn_of_counted(&reply);
            match turn {
                Turn::Answer(text) => {
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        // The reason picks the sentence: a token stop that reports "the
                        // steps ran out" sends the reader to the wrong setting.
                        break;
                    }
                    answer = Some(with_cut_off(text, reply.truncated));
                    break;
                }
                Turn::Calls(pending) => {
                    if rescued >= RESCUE_ROUNDS {
                        break;
                    }
                    rescued += 1;
                    let wire: Vec<Value> = pending.iter().map(PendingCall::to_wire).collect();
                    messages.push(ChatMessage::assistant_calls(
                        reply.content.trim().to_string(),
                        Value::Array(wire),
                    ));
                    for call in pending.iter() {
                        let call_id = call.id.clone();
                        let (name, result) = execute_call(
                            registry,
                            call,
                            &mut seen,
                            steps,
                            &mut calls_made,
                            &mut tracked,
                        )?;
                        messages.push(ChatMessage::tool(
                            call_id,
                            name,
                            model_json(&result, TOOL_RESULT_CHARS),
                        ));
                    }
                    messages.push(ChatMessage::user(prompts::AFTER_RESCUE));
                }
            }
        }
        answer.unwrap_or_else(|| stopped.fallback().to_string())
    };
    finish(
        &mut tracked,
        Outcome {
            content,
            steps,
            tool_calls: calls_made,
            tokens,
            stopped,
        },
    )
}

/// Has the run's clock run out? (`None` is a caller that bound its own time.)
fn spent(deadline: Option<Deadline>) -> bool {
    deadline.map_or(false, |deadline| deadline.remaining().is_none())
}

/// The answer, plus the one sentence that says the stream was cut off.
///
/// [`crate::chat::ChatReply::truncated`] is set when the SSE reader dropped a
/// half-written frame, and nothing read it: a connection that closed mid-answer
/// reached the reader as a finished thought. One sentence after the text is the
/// difference between "the model stopped talking" and "the model was still talking".
fn with_cut_off(content: String, truncated: bool) -> String {
    if truncated && !content.trim().is_empty() {
        format!("{}\n\n{}", content.trim_end(), prompts::CUT_OFF)
    } else {
        content
    }
}

/// Turn a callback's answer into a result: a cancel stops the whole run.
pub(super) fn check(control: Control) -> Result<(), AiError> {
    match control {
        Control::Continue => Ok(()),
        Control::Cancel => Err(AiError::Cancelled),
    }
}

fn finish(emit: &mut dyn FnMut(Event) -> Control, outcome: Outcome) -> Result<Outcome, AiError> {
    let _ = emit(Event::Done {
        content: outcome.content.clone(),
    });
    Ok(outcome)
}
