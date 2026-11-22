//! The reader's three numbers: what a request is built from, and what is said when a
//! number cannot be obeyed.

use super::*;
use crate::agent::AgentConfig;

/// The shipped numbers are the defaults, and the request they build is the one that ran
/// before this module existed: 32 768 of window, 8 192 of thought, 8 192 of answer.
#[test]
fn the_defaults_are_the_numbers_the_app_ran_on() {
    let budgets = Budgets::DEFAULT;
    assert_eq!(budgets.context, crate::context::CONTEXT_TOKENS);
    assert_eq!(budgets.reasoning, crate::chat::DEFAULT_MAX_TOKENS);
    assert_eq!(budgets.answer, crate::chat::DEFAULT_MAX_TOKENS);
    assert!(budgets.thinking());
    assert_eq!(
        budgets.max_tokens(),
        budgets.reasoning + budgets.answer,
        "one call carries the thought and the answer"
    );
}

/// The reader's answer budget is what a request carries when the model is not asked to
/// think: `model_answer: 4096` and `model_reasoning: 0` is a 4 096-token call.
#[test]
fn an_answer_budget_of_4096_is_a_4096_token_request() {
    let (budgets, notes) = Budgets::new(32_768, 0, 4096);
    assert!(notes.is_empty(), "both numbers are inside their ranges: {notes:?}");
    assert!(!budgets.thinking(), "reasoning 0 means do not think");
    assert_eq!(budgets.max_tokens(), 4096);
    let config = AgentConfig::default().using(&budgets);
    assert_eq!(config.max_tokens, 4096);
    assert_eq!(config.final_max_tokens, 4096);
    assert!(
        !config.enable_thinking,
        "no thinking block is requested when the reader set reasoning to 0"
    );
}

/// And with room to think, the call carries both: `reasoning 4096 + answer 4096`.
#[test]
fn a_thinking_turn_carries_the_thought_and_the_answer() {
    let (budgets, _) = Budgets::new(32_768, 4096, 4096);
    assert_eq!(budgets.max_tokens(), 8192);
    let config = AgentConfig::default().using(&budgets);
    assert!(config.enable_thinking);
    assert_eq!(config.max_tokens, 8192);
}

/// The trimming window follows `model_context` instead of the compiled-in constant: the
/// test that pins the removed literal.
#[test]
fn the_trimming_window_is_the_configured_context() {
    let (budgets, _) = Budgets::new(65_536, 8192, 8192);
    let config = AgentConfig::default().using(&budgets);
    assert_eq!(config.context, 65_536);
    assert_eq!(
        crate::context::prompt_room_in(config.context, 100_000, 8192),
        65_536 - 8192,
        "the window is the reader's, not CONTEXT_TOKENS"
    );
    assert_eq!(
        crate::context::prompt_room_in(32_768, 100_000, 8192),
        32_768 - 8192,
        "and the shipped window still answers for itself"
    );
    assert_eq!(
        budgets.prompt_tokens(),
        65_536 / 5,
        "a fifth of the reader's window is the prompt estimate"
    );
}

/// A number outside its range is clamped, never refused, and never silently: one sentence
/// per clamped key.
#[test]
fn a_number_outside_its_range_is_clamped_with_a_note() {
    let (budgets, notes) = Budgets::new(10, 1_000_000, 8);
    assert_eq!(budgets.context, *CONTEXT_RANGE.start());
    assert_eq!(budgets.reasoning, *REASONING_RANGE.end());
    assert_eq!(budgets.answer, *ANSWER_RANGE.start());
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(notes[0].contains("model_context 10"), "{notes:?}");
    assert!(notes[1].contains("model_reasoning 1000000"), "{notes:?}");
    assert!(notes[2].contains("model_answer 8"), "{notes:?}");
}

/// A config file with no model keys runs on the defaults; one with them is obeyed, even
/// when the table accessor does not know the key yet.
#[test]
fn the_config_file_is_obeyed_or_defaulted() {
    use atlas_config::{Config, Paths};
    use serde_json::json;

    let paths = Paths::at("/nonexistent-atlas-config-for-a-test");
    let built = |stored: serde_json::Map<String, serde_json::Value>| {
        Config::build(&paths, &[], &stored)
    };
    let empty = built(serde_json::Map::new());
    let (defaults, notes) = Budgets::from_config(&empty);
    assert_eq!(defaults, Budgets::DEFAULT, "{notes:?}");

    let mut stored = serde_json::Map::new();
    stored.insert("model_context".to_string(), json!(65_536));
    stored.insert("model_reasoning".to_string(), json!(1024));
    stored.insert("model_answer".to_string(), json!(2048));
    let (read, notes) = Budgets::from_config(&built(stored));
    assert_eq!(read.context, 65_536);
    assert_eq!(read.reasoning, 1024);
    assert_eq!(read.answer, 2048);
    assert!(notes.is_empty(), "{notes:?}");
}

/// A server that reports a smaller window than the config asks for is obeyed, and the
/// reader is told — not trimmed to in silence.
#[test]
fn a_smaller_reported_context_is_used_and_named() {
    let budgets = Budgets::new(32_768, 8192, 8192).0;
    let (same, silent) = budgets.against(Some(32_768));
    assert_eq!(same, budgets);
    assert!(silent.is_none());

    let (smaller, said) = budgets.against(Some(8192));
    assert_eq!(smaller.context, 8192);
    assert_eq!(smaller.reasoning, budgets.reasoning, "the other numbers stay");
    let said = said.expect("a sentence");
    assert!(said.contains("8192"), "{said}");
    assert!(said.contains("32768"), "{said}");
    assert!(said.ends_with('.'), "one sentence: {said}");

    let (unknown, nothing) = budgets.against(None);
    assert_eq!(unknown, budgets, "a server that does not say is not guessed at");
    assert!(nothing.is_none());
}
