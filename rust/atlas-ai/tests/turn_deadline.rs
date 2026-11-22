//! A turn's wall clock, through the public API the server calls.
//!
//! `THREAT-MODEL` §4.10 carried this as the known gap: the loop was bounded by steps
//! and tokens, both of which count *work*, and not by seconds. One model call has
//! `DEFAULT_TIMEOUT` of its own and a run is up to `MAX_STEPS + 1` of them, so on a
//! slow or wedged local model the reader's wait was the model's to decide.
//!
//! These tests drive `agent::run` and `agent::run_until` exactly as `atlas-web`'s
//! route and `atlas-ingest`'s research job do — public types only — so the shape the
//! route depends on is what is pinned here.

use std::cell::Cell;
use std::time::{Duration, Instant};

use atlas_ai::agent::{
    run, run_until, AgentConfig, Event, StopReason, OUT_OF_TIME, TURN_DEADLINE,
};
use atlas_ai::chat::{ChatReply, ChatRequest, LlamaClient};
use atlas_ai::deadline::Deadline;
use atlas_ai::error::AiError;
use atlas_ai::health::ServerInfo;
use atlas_ai::tool::ToolRegistry;
use atlas_ai::Control;

/// A model that never stops talking: every call streams a fragment and asks for
/// another while the loop lets it.
///
/// Its script is 500 fragments of its own, so a run with no deadline ends in a few
/// seconds with the wrong reason instead of hanging a test suite forever — a
/// regression has to fail, not stall.
struct NeverStops {
    calls: Cell<usize>,
}

impl LlamaClient for NeverStops {
    fn model(&self) -> &str {
        "never-stops"
    }

    fn complete(&self, _request: &ChatRequest) -> Result<ChatReply, AiError> {
        Err(AiError::Unreachable("this model only streams".to_string()))
    }

    fn complete_stream(
        &self,
        _request: &ChatRequest,
        on_delta: &mut dyn FnMut(&str) -> Control,
    ) -> Result<ChatReply, AiError> {
        self.calls.set(self.calls.get() + 1);
        for _ in 0..500 {
            if on_delta("tick ") == Control::Cancel {
                return Err(AiError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(ChatReply {
            model: "never-stops".to_string(),
            calls: Vec::new(),
            ..ChatReply::default()
        })
    }

    fn probe(&self) -> Result<ServerInfo, AiError> {
        Ok(ServerInfo {
            answering: true,
            model: "never-stops".to_string(),
            context_size: Some(6144),
            slots: Some(1),
        })
    }
}

/// A model with one answer and no socket.
struct OneAnswer(&'static str, Cell<usize>);

impl LlamaClient for OneAnswer {
    fn model(&self) -> &str {
        "scripted"
    }

    fn complete(&self, _request: &ChatRequest) -> Result<ChatReply, AiError> {
        self.1.set(self.1.get() + 1);
        Ok(ChatReply {
            model: "scripted".to_string(),
            content: self.0.to_string(),
            finish_reason: "stop".to_string(),
            ..ChatReply::default()
        })
    }

    fn complete_stream(
        &self,
        request: &ChatRequest,
        _on_delta: &mut dyn FnMut(&str) -> Control,
    ) -> Result<ChatReply, AiError> {
        self.complete(request)
    }

    fn probe(&self) -> Result<ServerInfo, AiError> {
        Ok(ServerInfo {
            answering: true,
            model: "scripted".to_string(),
            context_size: Some(6144),
            slots: Some(1),
        })
    }
}

fn config(max_steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps,
        ..AgentConfig::default()
    }
}

#[test]
fn a_turn_that_never_stops_is_cut_by_its_deadline() {
    // Measured before the deadline existed: this model ran both of the calls its step
    // budget allowed, 2.96 s, and the turn ended `EmptyAnswer`. With the real client
    // each of those calls may take `DEFAULT_TIMEOUT`, so the wait was the model's.
    //
    // The cut has to happen *inside* a call: that is where the waiting is.
    let client = NeverStops { calls: Cell::new(0) };
    let registry = ToolRegistry::new();
    let limits = AgentConfig {
        deadline: Some(Duration::from_millis(60)),
        ..config(3)
    };
    let started = Instant::now();
    let mut events = Vec::new();
    let outcome = run(&client, &registry, &limits, &[], "chips?", &mut |event| {
        events.push(event);
        Control::Continue
    })
    .expect("a cut turn is still an answer");
    let elapsed = started.elapsed();

    assert_eq!(
        outcome.stopped,
        StopReason::Deadline,
        "the clock, not the step budget, is what stopped this turn"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "the deadline was 60 ms and the cut took {:?}",
        elapsed
    );
    assert_eq!(client.calls.get(), 1, "no rescue call: there is no time for one");
    assert_eq!(outcome.steps, 1);
    // The fragments that did arrive are kept, and the sentence says why they stop.
    assert!(
        outcome.content.starts_with("tick"),
        "the answer so far is kept: {:?}",
        outcome.content
    );
    assert!(
        outcome.content.trim_end().ends_with(OUT_OF_TIME),
        "the reader is told the clock ran out: {:?}",
        outcome.content
    );
    assert!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Text { .. }))
            .count()
            > 1,
        "the reader saw it being written"
    );
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            content: outcome.content.clone()
        }),
        "the stream still ends with the answer it was given"
    );
}

#[test]
fn a_deadline_that_has_already_passed_stops_before_the_first_call() {
    // `run_until` is the entry point for a caller that holds its own deadline (a job
    // with a finish time, two runs sharing a budget). A spent one costs the model
    // nothing at all.
    let client = OneAnswer("never read", Cell::new(0));
    let registry = ToolRegistry::new();
    let outcome = run_until(
        &client,
        &registry,
        &config(5),
        &[],
        "chips?",
        &mut |_event| Control::Continue,
        Some(Deadline::new(Duration::ZERO)),
    )
    .expect("a spent clock is not an error");
    assert_eq!(outcome.stopped, StopReason::Deadline);
    assert_eq!(outcome.steps, 0, "no model turn was taken");
    assert_eq!(outcome.tool_calls, 0);
    assert_eq!(outcome.content, OUT_OF_TIME);
    assert_eq!(client.1.get(), 0, "the model was never asked");
}

#[test]
fn an_explicit_deadline_is_what_bounds_the_run() {
    // `run_until`'s deadline wins over the config's; `None` in both places is a run
    // with no clock, which only a caller that has its own bound should ask for.
    let client = OneAnswer("one line, quickly", Cell::new(0));
    let registry = ToolRegistry::new();
    let open = AgentConfig {
        deadline: None,
        ..config(5)
    };
    let outcome = run_until(
        &client,
        &registry,
        &open,
        &[],
        "chips?",
        &mut |_event| Control::Continue,
        Some(Deadline::new(Duration::from_secs(30))),
    )
    .expect("a run with time left answers");
    assert_eq!(outcome.stopped, StopReason::Answered);
    assert_eq!(outcome.content, "one line, quickly");

    let outcome = run(&client, &registry, &open, &[], "chips?", &mut |_event| {
        Control::Continue
    })
    .expect("a run with no clock answers too");
    assert_eq!(outcome.stopped, StopReason::Answered);
}

#[test]
fn the_default_config_carries_a_turn_deadline() {
    // The route and the research job both take `AgentConfig::default()`, so the
    // default is what closes the gap; a caller that wants its own number sets it.
    assert_eq!(AgentConfig::default().deadline, Some(TURN_DEADLINE));
    // As long as the calls a run may make: a thinking model spends minutes per
    // call, so the deadline is `MAX_STEPS + 1` calls, not one.
    assert_eq!(
        TURN_DEADLINE,
        atlas_ai::DEFAULT_TIMEOUT * (atlas_ai::agent::MAX_STEPS as u32 + 1)
    );
    assert!(!OUT_OF_TIME.contains('\n'), "{:?}", OUT_OF_TIME);
    assert!(OUT_OF_TIME.ends_with('.'), "{:?}", OUT_OF_TIME);
    assert_eq!(StopReason::Deadline.fallback(), OUT_OF_TIME);
    assert_ne!(
        StopReason::Deadline.fallback(),
        atlas_ai::agent::OUT_OF_TOKENS,
        "the clock is not the token budget"
    );
}
