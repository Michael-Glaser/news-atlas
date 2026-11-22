//! The agent loop: the model decides what to look up, the window looks it up, the model
//! answers.
//!
//! One question is not one request any more. The window offers [`crate::tools::specs`] to
//! the model, streams the turn to the page as it happens, and when the model answers with
//! tool calls instead of prose, runs them against the archive and asks again with their
//! results appended. What the page sees is one continuous turn: the thinking, a line naming
//! the tool that is running, and finally the answer.
//!
//! Two things this file deliberately does *not* do. It does not execute anything the model
//! names that the registry does not hold — [`crate::tools::call`] refuses an unknown name
//! before it opens the archive, and the refusal is handed back to the model as the tool's
//! result, so a hostile name becomes a sentence the model can read rather than an action.
//! And it does not retry a failed socket: a model that is not answering is one sentence to
//! the reader, not a loop that hides how long it has been failing.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::{http, tools};

/// The most reasoning this keeps across the steps of one turn.
///
/// Each step may think, and the page draws the whole chain; the cap is the same order as
/// one reply's, so a long turn cannot outgrow a short one by much.
const MAX_THINKING: usize = 200_000;

/// The most one turn may take, all steps together.
///
/// One thinking call measured on this machine took about four minutes, and a turn may take
/// a few of them; half an hour is the point where the reader has been waiting longer than
/// any answer is worth. The per-call bound is [`http::TURN_TIMEOUT`]; this one stops the
/// *loop*, and it is stated to the reader when it fires.
pub const DEADLINE: Duration = Duration::from_secs(30 * 60);

/// What one step of the turn produced, as the page hears it.
///
/// Deltas arrive while the model is working, a tool line arrives between steps, and exactly
/// one `Done` ends the turn — whether it answered, ran out, or failed.
pub enum Event {
    /// A piece of the stream, in the order the server sent it.
    Delta(http::Delta),
    /// The tool being run right now, as one line the page can show:
    /// `looking up: graph_neighbors China`.
    Tool(String),
    /// The end of the turn: the whole reply, or the one sentence that says why not.
    Done(Result<http::Reply, String>),
}

/// Run one question to its answer, reporting every step to `on` as it happens.
///
/// Blocking: the caller runs this off the drawing thread. `turns` is the conversation as
/// the page holds it, already carrying whatever archive block the page decided to send;
/// `system` is [`tools::SYSTEM`], which teaches the workflow. `budget` is the reader's
/// token budget, read from the archive by the app and handed down — every step of the turn
/// is sent with it, because the reader set one room for thinking and one for the answer,
/// and a step that quietly used a different one would make Settings a decoration.
pub fn run(
    archive: &Path,
    model_url: &str,
    system: &str,
    turns: Vec<http::Message>,
    budget: http::Budget,
    on: &mut dyn FnMut(Event),
) {
    let deadline = Instant::now() + DEADLINE;
    let mut conversation = turns;
    let mut thinking = String::new();
    for step in 0..=tools::MAX_STEPS {
        // The last step is asked without a menu: a model that has spent its steps looking
        // must now answer with what it has, and a tool it cannot call cannot stall it.
        let last = step == tools::MAX_STEPS;
        let menu = if last { Vec::new() } else { tools::specs() };
        let mut answer = String::new();
        let outcome = {
            let mut sink = |delta: http::Delta| match delta {
                http::Delta::Thinking(text) => {
                    keep(&mut thinking, &text);
                    on(Event::Delta(http::Delta::Thinking(text)));
                }
                http::Delta::Answer(text) => {
                    answer.push_str(&text);
                    on(Event::Delta(http::Delta::Answer(text)));
                }
            };
            http::ask_streaming(
                model_url,
                system,
                &conversation,
                &menu,
                budget,
                http::TURN_TIMEOUT,
                &mut sink,
            )
        };
        let reply = match outcome {
            Ok(reply) => reply,
            // One sentence, and the thinking that did arrive stays on the page.
            Err(sentence) => {
                on(Event::Done(Err(sentence)));
                return;
            }
        };
        if reply.tool_calls.is_empty() {
            let mut whole = reply;
            // The page drew the reasoning of every step; the finished line keeps all of it
            // rather than only the step that happened to answer.
            if whole.thinking.trim().is_empty() {
                whole.thinking = thinking;
            }
            on(Event::Done(Ok(whole)));
            return;
        }
        // The model showed its own request back on the next step, paired by id with the
        // result below: a turn whose calls are dropped looks to the model like a question it
        // never asked.
        let asked = if answer.trim().is_empty() {
            reply.answer.clone()
        } else {
            answer.clone()
        };
        conversation.push(http::Message::assistant_calls(
            asked,
            http::ToolCall::wire(&reply.tool_calls),
        ));
        for call in &reply.tool_calls {
            on(Event::Tool(format!("looking up: {}", call.label())));
            // A refused name, a bad argument and an empty answer are all the same thing to
            // the loop: text the model reads before it answers again.
            let result = match tools::call(archive, &call.name, &call.arguments) {
                Ok(answer) => answer,
                Err(sentence) => sentence,
            };
            conversation.push(http::Message::tool(call.id.clone(), result));
        }
        if Instant::now() >= deadline {
            on(Event::Done(Err(deadline_sentence())));
            return;
        }
    }
    // The loop's step budget and the last tool-free turn: reaching here means the model
    // asked for tools again even with no menu, which is not an answer.
    on(Event::Done(Err(deadline_sentence())));
}

/// Keep `piece` on the end of the reasoning kept across steps, up to the cap.
fn keep(held: &mut String, piece: &str) {
    if held.len() + piece.len() > MAX_THINKING {
        return;
    }
    held.push_str(piece);
}

fn deadline_sentence() -> String {
    format!(
        "The model was still working after {} minutes of looking things up, so this turn \
         stopped — ask a narrower question.",
        DEADLINE.as_secs() / 60
    )
}
