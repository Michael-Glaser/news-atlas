//! What one run carries: the budgets, the events, and the brake.
//!
//! Decisions: how many turns, how many tokens and how many seconds a run may spend
//! ([`MAX_STEPS`], [`TOTAL_TOKEN_BUDGET`], [`TURN_DEADLINE`], [`AgentConfig`]); what
//! a caller is told
//! as it happens ([`Event`]) and how that names itself on the wire
//! ([`event_json`]); why a run stopped ([`StopReason`]) and what it produced
//! ([`Outcome`]); and when a repeated tool call is a stuck model rather than
//! progress ([`RepeatDetector`] — the same calls as any recent step, whatever the
//! step).
//!
//! It also owns the two sentences a stopped run says: [`StopReason::guidance`],
//! what the model is told on the final no-tool call, and [`StopReason::fallback`],
//! what the reader is told when that call came back empty. They live beside the
//! enum because a reason and its wording are one decision: telling a reader the
//! *steps* ran out when the *tokens* did is a lie the type system can prevent.
//!
//! It does not: call the model, run a tool, or decide to stop — the loop in
//! [`crate::agent`] does that and reports through these types. It holds no state
//! between runs.

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::AiError;
use crate::prompts;
use crate::tool::ToolCall;

/// How many model turns one run may take — `ai.py`'s `max_steps=10`.
pub const MAX_STEPS: usize = 10;

/// Tokens one run may spend, prompt and reply estimated together.
///
/// `ai.py` had no such cap: ten steps of `max_tokens=2048` were the only bound.
/// The cap now has to hold a run of a **thinking** model: eleven calls (ten steps
/// plus the final one) of [`crate::context::PROMPT_TOKENS`] plus
/// [`crate::chat::DEFAULT_MAX_TOKENS`] is about 156 000 estimated tokens, so the
/// ceiling is 180 000. The old 24 000 stopped a researching run after its second
/// call and reported a budget that had never really been the model's — measured on
/// this machine, one classification with thinking on spends 4 633 completion
/// tokens, and the thought alone was 17 456 characters.
///
/// What it counts is the message list plus the reply. It does **not** count the
/// tool schemas, which [`crate::context::fit_context_with_tools`] charges to the
/// context window instead. Counting them twice would be double accounting for a
/// number a caller has already seen published (`/api/status`, and the UI mirrors
/// it), so the split is deliberate and written down in
/// [`Outcome::tokens`].
pub const TOTAL_TOKEN_BUDGET: u64 = 180_000;

/// `max_tokens` for the final answer, the one turn that may not call a tool.
///
/// It used to be 1 024, which is a typist's answer and not a reasoner's: the model
/// thinks before it writes, and a ceiling that fits only the thought comes back
/// with an empty `content` (measured: 4 633 completion tokens for one sentence of
/// JSON). The final turn is the one a reader waits for, so it gets the same room as
/// every other turn ([`crate::chat::DEFAULT_MAX_TOKENS`]).
pub const FINAL_MAX_TOKENS: u32 = crate::chat::DEFAULT_MAX_TOKENS;

/// How long one run may take, wall clock, whatever the model is doing.
///
/// The step and token budgets bound the *work* a turn may ask for, not the time it
/// takes to arrive: one model call has [`crate::DEFAULT_TIMEOUT`] (ten minutes) of
/// its own and a run is up to eleven of them, so a slow or wedged local model could
/// hold a reader for the better part of two hours with every other budget green.
/// A turn therefore has a clock, and it is as long as the calls it is allowed:
/// eleven × ten minutes is the outer bound, and this is `MAX_STEPS + 1` calls of
/// the same ceiling, floored so a run of a thinking model is not cut in half.
///
/// The number is measured, not guessed: on this machine one classification with
/// thinking on took **239 s** of wall clock for 4 633 completion tokens, so ten
/// tool-using steps of a research pass do not fit in ten minutes — the old ceiling
/// ended the run after the second call and told the reader the model was slow. A
/// caller that wants a tighter clock passes its own ([`AgentConfig::deadline`], or
/// [`crate::agent::run_until`]); `None` means no clock at all.
pub const TURN_DEADLINE: Duration = Duration::from_secs(
    crate::DEFAULT_TIMEOUT.as_secs() * (MAX_STEPS as u64 + 1),
);

/// What the reader is told when the turn's wall clock ran out.
///
/// The run keeps whatever the model had written and adds this sentence, because an
/// answer that stops mid-sentence without a word about why reads as a finished
/// thought that merely ended badly.
pub const OUT_OF_TIME: &str = "The model was still writing when this turn's time ran out, so the answer \
stops here. Ask a narrower question, so the next turn needs fewer steps.";

/// How many calls the final, tool-less turn may still get run.
///
/// The final call is made without tools so the reader gets an answer even when the step
/// or token budget ran out. The shipped model answers it with a tool call spelled as
/// text — measured twice, on a research pass that had read four articles and then wrote
/// `<function=save_article>` — which is work the run asked for and the archive never got.
/// Two rounds is enough to save what it read and to answer: the third would be a model
/// that will not write prose, and the fallback sentence is the honest end of that run.
pub const RESCUE_ROUNDS: usize = 2;

/// How many recent steps [`RepeatDetector`] remembers.
///
/// Two is not enough: a model that alternates two calls (`A`, `B`, `A`, `B`)
/// repeats every second step, and the whole step budget goes on re-reading two
/// results it already has. Eight is more than a real research pass needs to show
/// progress and small enough to keep the brake's memory trivial.
pub const REPEAT_WINDOW: usize = 8;

/// What the final no-tool call is told when the model kept asking for the same
/// call. Naming the situation is what lets the answer say what is missing instead
/// of starting the loop again.
pub const REPEAT_NUDGE: &str = "You already called that tool with those exact arguments; its result is in \
this conversation. Calling it again would return the same thing. Answer now in English with what you \
have, and name what is still missing.";

/// What the reader is told when the token budget ran out and the final call came
/// back empty. `prompts::OUT_OF_STEPS` would be a lie here: no step ceiling was
/// reached.
pub const OUT_OF_TOKENS: &str =
    "The run used its whole token budget before an answer was ready. Ask a narrower question, or start \
a new chat so the prompt is smaller.";

/// What the reader is told when the model looped and even the rescue call was
/// empty.
pub const REPEAT_FALLBACK: &str =
    "The model kept asking for the same tool result and had nothing to add. Ask again for one specific \
thing.";

/// What the model is told when it asks for a tool in a run that has none.
///
/// A caller may run the loop without handlers (the crate's own API allows it, and
/// so does `AgentConfig` with an empty registry). The model then still emits calls —
/// a grammar can force one — and the honest answer is the one that names the call it
/// did not make. The name is model output, so it is quoted with `{:?}` (control
/// characters escaped) and capped: nothing a model writes becomes a line of
/// instruction here.
pub fn no_tools_sentence(name: &str) -> String {
    let shown = crate::tool::shown_name(name);
    format!(
        "No tools are available in this run, so the call to {:?} was not made. Answer in one line \
         from what you already know, and say what is missing.",
        shown
    )
}

/// What the model is told when one turn asked for more calls than
/// [`crate::tool::MAX_CALLS_PER_TURN`].
///
/// The cap is what stops one answer becoming a thousand writes, and a cap the model
/// is not told about is a model that keeps doing it: this is the one sentence that
/// closes the turn. It is built from a count, never from model text, so it cannot be
/// made to say anything else.
pub fn calls_refused_sentence(refused: usize) -> String {
    format!(
        "This turn asked for {} more call(s). At most {} calls are made per turn, and only the \
         ones above were made. Ask for the rest in a later turn, one or two at a time.",
        refused,
        crate::tool::MAX_CALLS_PER_TURN
    )
}

/// The parameters one run uses.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// The system prompt. Defaults to [`prompts::SYSTEM`]; a caller may pass its
    /// own, and the loop never edits it.
    pub system: String,
    /// Model turns before the final answer.
    pub max_steps: usize,
    /// The window `llama-server` was started with, in tokens — the reader's
    /// `model_context` ([`crate::budget::Budgets::context`]). Trimming is measured against
    /// it, so a bigger configured window is actually used and not trimmed to the shipped
    /// constant.
    pub context: usize,
    /// Estimated prompt budget per call ([`crate::context::PROMPT_TOKENS`]).
    pub prompt_budget: usize,
    /// `max_tokens` for a turn that may call tools.
    pub max_tokens: u32,
    /// `max_tokens` for the final turn, without tools.
    pub final_max_tokens: u32,
    pub temperature: f32,
    /// Ceiling on estimated prompt + reply tokens for the whole run.
    pub total_token_budget: u64,
    /// Ceiling on the run's wall-clock time ([`TURN_DEADLINE`] by default);
    /// `None` runs without a clock, which only a caller that has its own bound
    /// should ask for.
    pub deadline: Option<Duration>,
    /// `tool_choice` sent with the tools.
    pub tool_choice: String,
    pub enable_thinking: bool,
    /// Tool results the trimming pass protects (0 = none protected).
    pub keep_recent: usize,
}

impl Default for AgentConfig {
    fn default() -> AgentConfig {
        AgentConfig {
            system: prompts::SYSTEM.to_string(),
            max_steps: MAX_STEPS,
            context: crate::context::CONTEXT_TOKENS,
            prompt_budget: crate::context::PROMPT_TOKENS,
            max_tokens: crate::chat::DEFAULT_MAX_TOKENS,
            final_max_tokens: FINAL_MAX_TOKENS,
            temperature: crate::chat::DEFAULT_TEMPERATURE,
            total_token_budget: TOTAL_TOKEN_BUDGET,
            deadline: Some(TURN_DEADLINE),
            tool_choice: "auto".to_string(),
            enable_thinking: true,
            keep_recent: 0,
        }
    }
}

impl AgentConfig {
    /// The same config, with the reader's three numbers instead of the defaults.
    ///
    /// One call asks for `reasoning + answer` tokens and thinking is requested only when
    /// the reader allowed a thought at all ([`crate::budget::Budgets::thinking`]): a
    /// request that says `enable_thinking: true` with no room to think is the starvation
    /// this project spent a day measuring. The machine guards — `max_steps`,
    /// `total_token_budget`, `deadline` — are left exactly as they are: they stop a wedged
    /// model, and a reader setting a bigger window is not a licence to run for hours.
    pub fn using(mut self, budgets: &crate::budget::Budgets) -> AgentConfig {
        self.context = budgets.context;
        self.prompt_budget = budgets.prompt_tokens();
        self.max_tokens = budgets.max_tokens();
        self.final_max_tokens = budgets.max_tokens();
        self.enable_thinking = budgets.thinking();
        self
    }
}

/// One typed step, for a server to stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A fragment of the model's text, as it arrived.
    Text { step: usize, delta: String },
    /// A validated tool call. Nothing malformed ever appears here.
    ToolCall { step: usize, call: ToolCall },
    /// What a tool answered. `error` is set for a rejected call or a failing
    /// handler; `repeated` is set when the same call was already run earlier in the
    /// run and its result was handed back instead of running it again.
    ToolResult {
        step: usize,
        name: String,
        args: Value,
        result: Value,
        error: bool,
        repeated: bool,
    },
    /// The answer.
    Done { content: String },
    /// The run stopped. Nothing follows this event.
    Error { error: AiError },
}

impl Event {
    /// A stable name for the wire, so a server does not match on variants.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Text { .. } => "text",
            Event::ToolCall { .. } => "tool_call",
            Event::ToolResult { .. } => "tool_result",
            Event::Done { .. } => "done",
            Event::Error { .. } => "error",
        }
    }
}

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The model answered without asking for a tool.
    Answered,
    /// The step budget ran out; the content came from the final no-tool call.
    StepsSpent,
    /// The token budget ran out.
    TokensSpent,
    /// The run's wall clock ([`TURN_DEADLINE`]) ran out while the model was still
    /// writing. There is no time left for a rescue call, so the content is what the
    /// model had written plus [`OUT_OF_TIME`].
    Deadline,
    /// The model asked for a call it had already made; the loop stopped offering
    /// tools and answered with what it had.
    RepeatedCall,
    /// The model answered nothing twice in a row.
    EmptyAnswer,
}

impl StopReason {
    /// The sentence the final no-tool call reads.
    ///
    /// One message per reason: a model that looped is told it looped (so the
    /// answer can say what is missing), and a model whose budget ran out is told
    /// which budget it was.
    pub fn guidance(&self) -> &'static str {
        match self {
            StopReason::RepeatedCall => REPEAT_NUDGE,
            // Never sent: nothing is asked of a model whose clock has run out. It is
            // the same sentence as the fallback rather than another reason's, so a
            // reader can never be told the wrong budget ran out.
            StopReason::Deadline => OUT_OF_TIME,
            _ => prompts::BUDGET_SPENT,
        }
    }

    /// What the reader is told when the final call produced no text.
    ///
    /// The reason decides the sentence. Saying "the steps ran out" after a token
    /// stop sends the reader looking for the wrong knob.
    pub fn fallback(&self) -> &'static str {
        match self {
            StopReason::StepsSpent => prompts::OUT_OF_STEPS,
            StopReason::TokensSpent => OUT_OF_TOKENS,
            StopReason::RepeatedCall => REPEAT_FALLBACK,
            StopReason::Deadline => OUT_OF_TIME,
            StopReason::EmptyAnswer | StopReason::Answered => prompts::EMPTY_ANSWER,
        }
    }
}

/// What one run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub content: String,
    /// Model turns taken, including the final no-tool call.
    pub steps: usize,
    /// Tool calls the model made that the registry accepted: executed, or answered
    /// from this run's cache because an identical call had already run. A call
    /// rejected before validation never ran, so it is not counted.
    pub tool_calls: usize,
    /// Estimated tokens the run spent: the message list it sent (per call, after
    /// trimming) plus the replies. The tool schemas are prompt tokens too, but they
    /// are the same bytes on every call and are charged to the context window
    /// instead ([`crate::context::fit_context_with_tools`]); see
    /// [`TOTAL_TOKEN_BUDGET`] for why they are not counted twice.
    pub tokens: u64,
    pub stopped: StopReason,
}

/// The same-call-twice detector.
///
/// `ai.py` answered a repeated call by handing back the cached result, which keeps
/// a stuck model alive until the step budget runs out. This one compares a step's
/// call keys with the last [`REPEAT_WINDOW`] steps': a step that asks for exactly
/// what an earlier step already asked for is a model that is not reacting to the
/// results, and the loop stops offering tools ([`StopReason::RepeatedCall`]).
///
/// Comparing against one step only would miss the common shape — `A`, `B`, `A`,
/// `B` — which is the same failure at a longer period. The system prompt already
/// forbids repeating an identical call, so the brake is enforcing a rule the model
/// was given.
///
/// The keys are [`crate::tool::PendingCall::key`]: the name plus a digest and a
/// length of the *raw* argument text, canonicalised. That is what makes the check
/// free — it happens before anything is validated or run, and it costs the same
/// whether the arguments are twenty bytes or a megabyte. Two spellings of one call
/// (`"limit": "5"` and `"limit": 5`) are therefore not the brake's business; they
/// are the run cache's, which keys on the validated arguments
/// (`crate::agent::step`).
#[derive(Debug, Default)]
pub struct RepeatDetector {
    recent: Vec<Vec<String>>,
}

impl RepeatDetector {
    pub fn new() -> RepeatDetector {
        RepeatDetector::default()
    }

    /// Record this step's calls; `true` means "an earlier step asked for exactly
    /// this, so this is a loop, not progress".
    ///
    /// An empty step is never a repeat (it cannot happen — a `Turn::Calls` has at
    /// least one call — and pretending it could would make the brake fire on
    /// nothing).
    pub fn observe(&mut self, keys: Vec<String>) -> bool {
        let mut sorted = keys;
        sorted.sort();
        if sorted.is_empty() {
            return false;
        }
        if self.recent.iter().any(|seen| *seen == sorted) {
            return true;
        }
        if self.recent.len() >= REPEAT_WINDOW {
            self.recent.remove(0);
        }
        self.recent.push(sorted);
        false
    }

    /// Forget every step (a text-only turn breaks the pattern).
    pub fn reset(&mut self) {
        self.recent.clear();
    }

    /// How many steps the brake is holding, for a caller's own assertion.
    pub fn len(&self) -> usize {
        self.recent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }
}

/// The JSON one event becomes on the wire. `Text` carries only its fragment: a
/// server that wants the whole answer accumulates the `Done` content.
pub fn event_json(event: &Event) -> Value {
    match event {
        Event::Text { step, delta } => json!({ "type": "text", "step": step, "delta": delta }),
        Event::ToolCall { step, call } => {
            json!({ "type": "tool_call", "step": step, "call": call.to_json() })
        }
        Event::ToolResult {
            step,
            name,
            args,
            result,
            error,
            repeated,
        } => json!({
            "type": "tool_result",
            "step": step,
            "name": name,
            "args": args,
            "result": result,
            "error": error,
            "repeated": repeated,
        }),
        Event::Done { content } => json!({ "type": "done", "content": content }),
        Event::Error { error } => {
            let mut value = error.to_json();
            if let Some(object) = value.as_object_mut() {
                object.insert("type".to_string(), Value::String("error".to_string()));
            }
            value
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    #[test]
    fn a_repeat_detector_ignores_a_text_turn() {
        let mut detector = RepeatDetector::new();
        assert!(!detector.observe(vec!["a".to_string()]));
        assert!(detector.observe(vec!["a".to_string()]));
        detector.reset();
        assert!(!detector.observe(vec!["a".to_string()]));
        assert!(!detector.observe(Vec::new()), "an empty step is not a repeat");
    }

    #[test]
    fn the_brake_sees_a_loop_at_a_longer_period_too() {
        // A, B, A, B: every second step repeats one from two steps back. Comparing
        // with the previous step alone let this spend the whole budget.
        let mut detector = RepeatDetector::new();
        assert!(!detector.observe(vec!["a".to_string()]));
        assert!(!detector.observe(vec!["b".to_string()]));
        assert!(detector.observe(vec!["a".to_string()]), "A again is a loop");
        detector.reset();
        assert!(!detector.observe(vec!["a".to_string()]), "a reset forgets every step");
    }

    #[test]
    fn the_brake_keeps_a_bounded_memory_and_forgets_the_oldest() {
        let mut detector = RepeatDetector::new();
        for index in 0..REPEAT_WINDOW {
            assert!(!detector.observe(vec![format!("call{}", index)]));
        }
        assert_eq!(detector.len(), REPEAT_WINDOW);
        // The newest step pushes the oldest out...
        assert!(!detector.observe(vec!["fresh".to_string()]));
        assert_eq!(detector.len(), REPEAT_WINDOW, "the window does not grow");
        // ...so the very first call is no longer remembered, and a fresh one is.
        assert!(!detector.observe(vec!["call0".to_string()]));
        assert!(detector.observe(vec!["fresh".to_string()]));
    }

    #[test]
    fn a_reason_chooses_its_own_two_sentences() {
        // The reader is never told the wrong budget ran out.
        assert_eq!(
            StopReason::TokensSpent.fallback(),
            OUT_OF_TOKENS,
            "a token stop must not say the steps ran out"
        );
        assert_eq!(StopReason::StepsSpent.fallback(), prompts::OUT_OF_STEPS);
        assert_eq!(StopReason::RepeatedCall.fallback(), REPEAT_FALLBACK);
        assert_ne!(
            StopReason::StepsSpent.fallback(),
            StopReason::TokensSpent.fallback()
        );
        // The model is told something it can act on, and it names the situation.
        assert!(StopReason::RepeatedCall.guidance().contains("exact arguments"));
        assert_eq!(
            StopReason::StepsSpent.guidance(),
            prompts::BUDGET_SPENT,
            "the tuned sentence stays for the two budget stops"
        );
        assert_eq!(StopReason::TokensSpent.guidance(), prompts::BUDGET_SPENT);
        // The three sentences a reader can end up reading are single lines: a
        // line-continuation that leaves a double space or a stray newline in the
        // copy is the sort of thing nobody notices until it is on screen.
        for sentence in [
            OUT_OF_TOKENS,
            REPEAT_FALLBACK,
            REPEAT_NUDGE,
            no_tools_sentence("x").as_str(),
            calls_refused_sentence(1).as_str(),
            calls_refused_sentence(992).as_str(),
        ] {
            assert!(!sentence.contains('\n'), "{:?}", sentence);
            assert!(!sentence.contains("  "), "{:?}", sentence);
            assert!(sentence.ends_with('.'), "{:?}", sentence);
        }
    }

    #[test]
    fn every_event_has_a_wire_form() {
        let call = ToolCall {
            id: "1".to_string(),
            name: "stats".to_string(),
            args: Map::new(),
            source: crate::tool::CallSource::Native,
        };
        let events = vec![
            Event::Text {
                step: 1,
                delta: "x".to_string(),
            },
            Event::ToolCall {
                step: 1,
                call,
            },
            Event::ToolResult {
                step: 1,
                name: "stats".to_string(),
                args: json!({}),
                result: json!({ "ok": true }),
                error: false,
                repeated: false,
            },
            Event::Done {
                content: "x".to_string(),
            },
            Event::Error {
                error: AiError::Loop("r".to_string()),
            },
        ];
        let kinds: Vec<&str> = events.iter().map(Event::kind).collect();
        assert_eq!(
            kinds,
            vec!["text", "tool_call", "tool_result", "done", "error"]
        );
        for event in events {
            let wire = event_json(&event);
            assert_eq!(wire["type"], json!(event.kind()));
        }
    }
}
