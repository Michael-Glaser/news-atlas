//! Keeping a message list inside the model's small context window.
//!
//! Ported from `ai.py`, rule for rule, because the rules are what make a small
//! window survive a tool loop:
//!
//! * a tool result is handed to the model as compact JSON, capped at
//!   [`TOOL_RESULT_CHARS`]; long strings (article text) are cut with a marker that
//!   says the full text still exists server-side, so the model does not try to
//!   repeat it in an argument ([`model_json`]);
//! * the whole message list is estimated at four bytes per token
//!   ([`estimate_tokens`]) — crude, and the same crude number Python used, so a
//!   tuned prompt budget keeps its meaning;
//! * the tool *schemas* are prompt too. `ai.py` forgot that: it compared the
//!   message estimate against a budget and sent 23 schemas on top. Here the
//!   schema array is estimated the same way ([`estimate_value_tokens`]) and
//!   subtracted from the budget before any message is measured
//!   ([`fit_context_with_tools`]), so "the prompt fits" is a claim about the whole
//!   request: system prompt + schemas + messages + the reply's reserve
//!   ([`prompt_room`]);
//! * when the estimate is over budget, **old tool results are replaced first**,
//!   oldest first, and the last `keep_recent` of them are protected. The system
//!   prompt and the reader's own messages are never touched: they are not
//!   candidates. If stubbing every tool result still leaves the list over budget,
//!   [`Fit::over_budget`] says so instead of pretending, and the loop retries with
//!   half the budget exactly as `ai.py` did.
//! * what the UI shows is a different cap ([`truncate_for_ui`]): 40 keys, 30
//!   items, 800 characters per string.
//!
//! # The byte budgets, in one place
//!
//! | Budget | Bytes | Tokens (÷4) | Who it bounds |
//! | --- | --- | --- | --- |
//! | [`TOOL_RESULT_CHARS`] | 3 500 | ≈ 875 | one tool result on its way to the model |
//! | [`MAX_ARG_CHARS`] | 40 000 | ≈ 10 000 | one string argument the model writes |
//! | [`MAX_ARG_ITEM`] | 200 | ≈ 50 | one item of an array argument |
//! | [`PROMPT_TOKENS`] | — | 5 000 | system + schemas + messages, per call |
//! | [`CONTEXT_TOKENS`] | — | 32 768 | the default window ([`crate::budget::Budgets`] carries the configured one) |
//!
//! A tool result is capped *before* it enters the message list, and the list is
//! trimmed *before* the request is sent: two passes, in that order, so the second
//! one never has to guess what the first one did.

use serde_json::{Map, Value};

use crate::chat::ChatMessage;

/// The window the model server is started with when `data/config.json` says nothing, in
/// tokens — the **default** for `model_context`, not the number every run uses.
///
/// Raised from 8 192 to 32 768 because the model thinks before it answers and a page of
/// reasoning is most of a small window: a 5 302-character article plus a 3 533-character
/// thought plus the answer is more than 8 192 with the tool schemas in front.
///
/// The window is a promise the *server* has to keep (`llama-server -c`), which is why it
/// is a reader setting now ([`crate::budget::Budgets::context`]): this constant is what a
/// run falls back to, and `Budgets::against` says so when the server reports a smaller
/// one. Trimming decisions take the window from the run's budgets; only callers with no
/// window of their own still measure against this.
pub const CONTEXT_TOKENS: usize = 32768;

/// `ai.py::TOOL_RESULT_CHARS` — one tool result, as JSON, handed back to the model.
pub const TOOL_RESULT_CHARS: usize = 3500;

/// `ai.py::MAX_ARG_CHARS` — one model-supplied string argument (article text
/// needs room).
pub const MAX_ARG_CHARS: usize = 40_000;

/// `ai.py::MAX_ARG_ITEM` — one item of a model-supplied array (a tag, a category).
pub const MAX_ARG_ITEM: usize = 200;

/// `ai.py::MAX_ARG_ITEMS` — items per array argument.
pub const MAX_ARG_ITEMS: usize = 32;

/// The estimate the whole message list may take, in tokens.
///
/// A fifth of the window: the reply (including its thinking) gets the rest, which is what
/// makes an eight-thousand-token thought answerable instead of truncated.
pub const PROMPT_TOKENS: usize = 6000;

/// The replacement for a tool result that was dropped to make room. Verbatim:
/// the model must read it as a fact about the context, not as a failed tool.
pub const TOOL_STUB: &str = "{\"trimmed\": true, \"note\": \"older tool result removed to save context - \
re-run the tool if you still need it\"}";

/// `ai.py::_CUT` — appended to a string that was shortened for the model.
pub const CUT: &str = " … [truncated - full result is cached server-side]";

/// Characters kept when a string is halved, so a cut string stays informative.
const MIN_KEEP: usize = 160;

/// How many halving passes [`model_json`] may make before giving up and sending a
/// preview instead. `ai.py` used 200.
const MAX_CUTS: usize = 200;

// ---------------------------------------------------------------------------
// estimation
// ---------------------------------------------------------------------------

/// Four bytes per token, over the serialised message list — `ai.py`'s estimate.
///
/// It is not a tokenizer and it is not meant to be: it is the same number the
/// Python layer compared against the same budget, so the same prompts fit or do
/// not fit as before.
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    match serde_json::to_string(messages) {
        Ok(text) => text.len() / 4,
        Err(_) => 0,
    }
}

/// Four bytes per token over any JSON value's compact text — the same estimate as
/// [`estimate_tokens`], for the half of the prompt that is not a message.
///
/// The tool array is prompt in exactly the same sense the messages are: llama.cpp
/// puts the schemas in front of the conversation, and they occupy the same window.
/// Estimating them with one function keeps the two halves comparable.
pub fn estimate_value_tokens(value: &Value) -> usize {
    match serde_json::to_string(value) {
        Ok(text) => text.len() / 4,
        Err(_) => 0,
    }
}

/// The room a message list may take in one call, in the shipped window.
///
/// Kept for callers that have no window of their own (a test, a tool). A run uses
/// [`prompt_room_in`] with the window the reader configured ([`crate::budget::Budgets`]),
/// because the number here is only a default and llama.cpp's `-c` is what actually holds.
pub fn prompt_room(budget: usize, max_tokens: u32) -> usize {
    prompt_room_in(CONTEXT_TOKENS, budget, max_tokens)
}

/// The room a message list may take in one call, given the window `llama-server` runs
/// with (`-c`, the reader's `model_context`).
///
/// The caller's prompt budget, but never more than the window minus what the reply may
/// use: `prompt + max_tokens <= context`, or llama.cpp answers `400` and the loop has to
/// retry with half the budget. A caller that raises `prompt_budget` past the window gets
/// the window's answer instead of a refused request, and a window smaller than the reply
/// gets 0 — the trimming pass then drops tool results to stubs and the call is still made,
/// with the smallest prompt it can build.
pub fn prompt_room_in(context: usize, budget: usize, max_tokens: u32) -> usize {
    let window = context.saturating_sub(max_tokens as usize);
    budget.min(window)
}

/// What [`fit_context`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// The estimate of the messages after trimming.
    pub tokens: usize,
    /// How many tool results were replaced by [`TOOL_STUB`].
    pub trimmed: usize,
    /// Still over the budget with nothing left to trim.
    pub over_budget: bool,
    /// The estimate of the tool schemas sent with this request, which the budget
    /// had to cover first.
    pub tools: usize,
}

impl Fit {
    /// The estimate of the whole prompt: schemas plus messages.
    ///
    /// This is the number the context window is measured against. `tokens` alone
    /// is what the run's own counter adds up (see [`crate::agent::Outcome::tokens`]).
    pub fn prompt_tokens(&self) -> usize {
        self.tokens + self.tools
    }
}

/// Replace old tool results with [`TOOL_STUB`] until the list fits `budget`.
///
/// Priority, in order: the system prompt and the user/assistant turns are never
/// candidates; the last `keep_recent` tool results are trimmed last; everything
/// older goes oldest-first. The estimate is recomputed after every replacement,
/// so the loop stops as soon as it fits instead of trimming to the bone.
pub fn fit_context(messages: &mut [ChatMessage], budget: usize, keep_recent: usize) -> Fit {
    fit_context_with_tools(messages, None, budget, keep_recent)
}

/// [`fit_context`], with the tool schemas counted against the same budget.
///
/// `tools` is the array [`crate::tool::ToolRegistry::wire`] produces (or `None` for
/// a final call that offers no tools). The schemas are not trimmable — the model
/// needs all 23 or a tool becomes uncallable — so they come out of the budget
/// first and the messages get the rest. `over_budget` is `true` when even an empty
/// message list plus the schemas does not fit.
pub fn fit_context_with_tools(
    messages: &mut [ChatMessage],
    tools: Option<&Value>,
    budget: usize,
    keep_recent: usize,
) -> Fit {
    let tools = tools.map(estimate_value_tokens).unwrap_or(0);
    let room = budget.saturating_sub(tools);
    let tool_index: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == "tool")
        .map(|(index, _)| index)
        .collect();
    let protected_from = tool_index.len().saturating_sub(keep_recent);
    let mut order: Vec<usize> = Vec::with_capacity(tool_index.len());
    order.extend_from_slice(&tool_index[..protected_from]);
    order.extend_from_slice(&tool_index[protected_from..]);

    let mut trimmed = 0usize;
    let mut tokens = estimate_tokens(messages);
    for index in order {
        if tokens <= room {
            break;
        }
        if messages[index].content != TOOL_STUB {
            messages[index].content = TOOL_STUB.to_string();
            trimmed += 1;
            tokens = estimate_tokens(messages);
        }
    }
    Fit {
        tokens,
        trimmed,
        over_budget: tokens + tools > budget,
        tools,
    }
}

// ---------------------------------------------------------------------------
// tool results for the model
// ---------------------------------------------------------------------------

/// Compact JSON of a tool result, capped at `limit` bytes — still valid JSON.
///
/// Long strings are halved (never below `MIN_KEEP` characters, always with
/// [`CUT`] appended) before anything structural is dropped, so the model sees
/// "the text is longer than this and lives on disk" rather than a JSON cut in the
/// middle of a key. A result with nothing left to halve (500 one-word keys, say)
/// becomes a `preview` object instead.
///
/// The cap is a **byte** cap and it holds: the preview is shrunk until the whole
/// body fits, because a budget the caller cannot rely on is not a budget. That is
/// what makes [`TOOL_RESULT_CHARS`] a number a reader can check against the window.
pub fn model_json(result: &Value, limit: usize) -> String {
    let mut node = result.clone();
    let mut text = match serde_json::to_string(&node) {
        Ok(text) => text,
        Err(_) => return "{\"error\":\"unserialisable tool result\"}".to_string(),
    };
    for _ in 0..MAX_CUTS {
        if text.len() <= limit {
            return text;
        }
        let mut path: Vec<Step> = Vec::new();
        let mut best: Option<(usize, Vec<Step>)> = None;
        find_longest(&node, &mut path, &mut best);
        let (length, path) = match best {
            Some(found) => found,
            None => break,
        };
        if path.is_empty() {
            break;
        }
        let keep = (length / 2).max(MIN_KEEP);
        cut_at(&mut node, &path, keep);
        text = match serde_json::to_string(&node) {
            Ok(text) => text,
            Err(_) => break,
        };
    }
    // The fallback: a preview of what could not be trimmed, in a body that fits.
    let mut keep = (limit.saturating_sub(240)).max(MIN_KEEP);
    for _ in 0..PREVIEW_FITS {
        let body = preview_body(&text, keep);
        let length = body.len();
        if length <= limit {
            return body;
        }
        // The wrapper cost is whatever the body is longer than its preview by;
        // taking it off `keep` once is exact enough, two passes are certain.
        let over = length - limit;
        if keep <= over + MIN_KEEP {
            keep = MIN_KEEP.min(keep);
            let smallest = preview_body(&text, keep);
            return if smallest.len() <= limit {
                smallest
            } else {
                // Nothing left to shrink: an honest, tiny object still parses.
                "{\"truncated\":true,\"note\":\"this result is too large for the context window\"}"
                    .to_string()
            };
        }
        keep -= over;
    }
    preview_body(&text, keep)
}

/// How many times the preview shrinker may try to reach the limit. Two passes:
/// one to measure the wrapper, one to confirm.
const PREVIEW_FITS: usize = 3;

/// The JSON object a result too big for the context becomes.
fn preview_body(text: &str, keep: usize) -> String {
    let preview = crate::prompts::cut_chars(text, keep);
    Value::Object({
        let mut map = Map::new();
        map.insert("truncated".to_string(), Value::Bool(true));
        map.insert(
            "note".to_string(),
            Value::String(
                "this result is too large for the context window - the full data stays cached \
                 server-side, do not try to repeat it"
                    .to_string(),
            ),
        );
        map.insert("preview".to_string(), Value::String(preview));
        map
    })
    .to_string()
}

/// Where a value sits inside a JSON tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

/// The longest string inside `node`, with its path, so it can be cut first.
fn find_longest(node: &Value, path: &mut Vec<Step>, best: &mut Option<(usize, Vec<Step>)>) {
    match node {
        Value::String(text) => {
            let length = text.chars().count();
            let better = match best {
                Some((best_len, _)) => length > *best_len,
                None => length > 0,
            };
            if better {
                *best = Some((length, path.clone()));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(Step::Index(index));
                find_longest(item, path, best);
                path.pop();
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                path.push(Step::Key(key.clone()));
                find_longest(item, path, best);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Cut the string at `path` to `keep` characters plus [`CUT`].
fn cut_at(node: &mut Value, path: &[Step], keep: usize) {
    let (head, tail) = match path.split_first() {
        Some(pair) => pair,
        None => return,
    };
    let child = match head {
        Step::Key(key) => node.get_mut(key),
        Step::Index(index) => node.get_mut(*index),
    };
    let child = match child {
        Some(child) => child,
        None => return,
    };
    if tail.is_empty() {
        if let Value::String(text) = child {
            let mut cut = crate::prompts::cut_chars(text, keep);
            cut.push_str(CUT);
            *text = cut;
        }
        return;
    }
    cut_at(child, tail, keep);
}

/// What the browser gets: 40 keys, 30 items, 800 characters — `ai.py::_truncate`.
///
/// The UI keeps the whole result within these caps; the model gets
/// [`model_json`], which is much smaller than either. Keeping the two caps apart
/// is what lets the reader see an article the model was only told about.
pub fn truncate_for_ui(node: &Value) -> Value {
    const MAX_KEYS: usize = 40;
    const MAX_ITEMS: usize = 30;
    const MAX_STRING: usize = 800;
    match node {
        Value::Object(map) => {
            let mut out = Map::new();
            for (index, (key, value)) in map.iter().enumerate() {
                if index >= MAX_KEYS {
                    break;
                }
                out.insert(key.clone(), truncate_for_ui(value));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(MAX_ITEMS)
                .map(truncate_for_ui)
                .collect::<Vec<Value>>(),
        ),
        Value::String(text) => {
            if text.chars().count() > MAX_STRING {
                let mut cut = crate::prompts::cut_chars(text, MAX_STRING);
                cut.push('…');
                Value::String(cut)
            } else {
                Value::String(text.clone())
            }
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The array the loop actually puts in `tools`: all 23 schemas, through the
    /// registry's own wire form.
    fn all_schemas() -> Value {
        let mut registry = crate::tool::ToolRegistry::new();
        for spec in crate::specs::tools() {
            registry.register_spec(spec);
        }
        registry.wire()
    }

    #[test]
    fn estimation_is_bytes_over_four() {
        let messages = vec![ChatMessage::user("abcd")];
        let bytes = serde_json::to_string(&messages).unwrap().len();
        assert_eq!(estimate_tokens(&messages), bytes / 4);
    }

    #[test]
    fn trimming_keeps_the_system_prompt_and_the_user_message() {
        let system = ChatMessage::system("S".repeat(4000));
        let user = ChatMessage::user("U".repeat(4000));
        let mut messages = vec![
            system.clone(),
            user.clone(),
            ChatMessage::tool("c0", "search_news", "A".repeat(40_000)),
            ChatMessage::assistant("thinking"),
            ChatMessage::tool("c1", "get_article", "B".repeat(40_000)),
        ];
        let fit = fit_context(&mut messages, PROMPT_TOKENS, 0);
        assert!(fit.trimmed > 0, "something had to be dropped");
        assert_eq!(messages[0], system, "the system prompt is never touched");
        assert_eq!(messages[1], user, "the reader's message is never touched");
        assert_eq!(messages[2].content, TOOL_STUB, "the oldest tool result goes first");
        assert!(!fit.over_budget, "the list now fits");
        assert!(fit.tokens <= PROMPT_TOKENS);
    }

    #[test]
    fn the_newest_tool_results_are_trimmed_last() {
        let mut messages = vec![
            ChatMessage::system("s"),
            ChatMessage::user("u"),
            ChatMessage::tool("c0", "a", "A".repeat(30_000)),
            ChatMessage::tool("c1", "b", "B".repeat(30_000)),
            ChatMessage::tool("c2", "c", "C".repeat(30_000)),
        ];
        // Enough budget for one big result plus the small turns.
        let fit = fit_context(&mut messages, 9000, 1);
        assert_eq!(messages[2].content, TOOL_STUB, "oldest goes first");
        assert_eq!(messages[3].content, TOOL_STUB, "then the next");
        assert_ne!(messages[4].content, TOOL_STUB, "the newest is protected");
        assert!(fit.trimmed >= 2);
    }

    #[test]
    fn a_list_that_cannot_fit_says_so() {
        let mut messages = vec![
            ChatMessage::system("S".repeat(40_000)),
            ChatMessage::user("U".repeat(40_000)),
        ];
        let fit = fit_context(&mut messages, PROMPT_TOKENS, 0);
        assert_eq!(fit.trimmed, 0);
        assert!(fit.over_budget, "no tool result to drop, so it cannot fit");
        assert_eq!(fit.tools, 0, "no tools were offered");
    }

    #[test]
    fn fit_context_without_tools_is_the_python_rule_unchanged() {
        // The old entry point must keep its old meaning: the whole budget is for
        // the messages. Callers that pass no tools (the final call) rely on it.
        let messages = vec![
            ChatMessage::system("s"),
            ChatMessage::user("u"),
            ChatMessage::tool("c0", "a", "A".repeat(30_000)),
        ];
        let mut with_none = messages.clone();
        let mut with_tools = messages.clone();
        let plain = fit_context(&mut with_none, 9000, 0);
        let delegated = fit_context_with_tools(&mut with_tools, None, 9000, 0);
        assert_eq!(plain, delegated);
        assert_eq!(plain.tools, 0);
        assert_eq!(plain.prompt_tokens(), plain.tokens);
    }

    #[test]
    fn the_tool_schemas_come_out_of_the_budget_before_any_message() {
        // 23 schemas are prompt. A budget that covers them leaves less room for the
        // message list, and the trimming must know that — otherwise the request is
        // schemas + 5000 tokens of messages, which is what `ai.py` actually sent.
        let tools = all_schemas();
        let schema_tokens = estimate_value_tokens(&tools);
        assert!(schema_tokens > 0);
        let template: Vec<ChatMessage> = vec![
            ChatMessage::system("s"),
            ChatMessage::user("u"),
            ChatMessage::tool("c0", "a", "A".repeat(8_000)),
            ChatMessage::tool("c1", "b", "B".repeat(8_000)),
        ];
        let budget = PROMPT_TOKENS;

        let mut without = template.clone();
        let plain = fit_context(&mut without, budget, 0);
        assert_eq!(plain.trimmed, 0, "the messages alone fit the budget");

        let mut with = template.clone();
        let fit = fit_context_with_tools(&mut with, Some(&tools), budget, 0);
        assert_eq!(fit.tools, schema_tokens);
        assert_eq!(fit.prompt_tokens(), fit.tokens + schema_tokens);
        assert!(
            fit.prompt_tokens() <= budget,
            "schemas + messages must fit the budget: {} > {}",
            fit.prompt_tokens(),
            budget
        );
        assert!(
            fit.trimmed > plain.trimmed,
            "the schemas took the room the tool results had: {} > {}",
            fit.trimmed,
            plain.trimmed
        );
        assert!(schema_tokens < budget, "the schemas leave room to work in");
    }

    #[test]
    fn a_budget_smaller_than_the_schemas_is_over_budget_not_a_guess() {
        let tools = all_schemas();
        let schema_tokens = estimate_value_tokens(&tools);
        let mut messages = vec![ChatMessage::system("s"), ChatMessage::user("u")];
        let fit = fit_context_with_tools(&mut messages, Some(&tools), schema_tokens - 1, 0);
        assert!(fit.over_budget, "the schemas alone do not fit");
        assert_eq!(fit.trimmed, 0, "there was nothing to trim");
        assert_eq!(
            messages[0].content, "s",
            "a hopeless budget must not shred the conversation"
        );
    }

    #[test]
    fn prompt_room_never_leaves_the_reply_outside_the_window() {
        // The shipped pair: a 6 000-token prompt and an 8 192-token reply (the thinking
        // budget) fit the 32 768-token window with room to spare.
        assert_eq!(
            prompt_room(PROMPT_TOKENS, 8192),
            PROMPT_TOKENS,
            "a 6 000-token prompt plus an 8 192-token reply still fits the 32 768 window"
        );
        // A prompt larger than the window leaves exactly the reply's room.
        assert_eq!(prompt_room(100_000, 8192), CONTEXT_TOKENS - 8192);
        // A reply larger than the whole window leaves nothing.
        assert_eq!(
            prompt_room(100_000, (CONTEXT_TOKENS + 1) as u32),
            0,
            "a reply may not be larger than the window"
        );
        assert_eq!(prompt_room(1000, 600), 1000);
    }

    #[test]
    fn model_json_caps_a_result_and_marks_the_cut() {
        let result = json!({ "id": 7, "text": "y".repeat(20_000) });
        let text = model_json(&result, TOOL_RESULT_CHARS);
        assert!(text.len() <= TOOL_RESULT_CHARS, "{} bytes", text.len());
        assert!(text.contains("cached server-side") || text.contains("too large"));
        // Still valid JSON: the loop puts it in a message, the model must parse it.
        assert!(serde_json::from_str::<Value>(&text).is_ok());
    }

    #[test]
    fn model_json_leaves_a_small_result_alone() {
        let result = json!({ "ok": true, "items": [1, 2, 3] });
        let text = model_json(&result, TOOL_RESULT_CHARS);
        assert_eq!(text, serde_json::to_string(&result).unwrap());
    }

    #[test]
    fn model_json_gives_up_gracefully_on_a_huge_flat_list() {
        let many: Vec<Value> = (0..500).map(|i| Value::String(format!("{}", i))).collect();
        let text = model_json(&Value::Array(many), 400);
        assert!(serde_json::from_str::<Value>(&text).is_ok());
        // The cap is a byte cap, not a hope: this used to land at ~1 180 bytes for a
        // 400-byte limit, which is a budget a caller cannot reason about.
        assert!(text.len() <= 400, "{} bytes for a 400-byte limit", text.len());
    }

    #[test]
    fn the_byte_budget_holds_for_a_result_that_cannot_be_halved() {
        // Nothing in here is a long string, so no halving pass can help: the preview
        // path has to honour the cap on its own. Escape-heavy text is the worst
        // case, because every quote costs two bytes in JSON.
        let mut keys: Map<String, Value> = Map::new();
        for index in 0..300 {
            keys.insert(format!("k{}", index), Value::String(format!("v{}", index)));
        }
        let wide = Value::Object(keys);
        let text = model_json(&wide, TOOL_RESULT_CHARS);
        assert!(text.len() <= TOOL_RESULT_CHARS, "{} bytes", text.len());
        assert!(serde_json::from_str::<Value>(&text).is_ok(), "still JSON");

        let escaped = json!({ "page": "\"quoted\" \\ back\\slash ".repeat(4_000) });
        for limit in [400usize, 800, TOOL_RESULT_CHARS] {
            let text = model_json(&escaped, limit);
            assert!(
                text.len() <= limit,
                "{} bytes for a {} limit: {}",
                text.len(),
                limit,
                &text[..80.min(text.len())]
            );
            assert!(serde_json::from_str::<Value>(&text).is_ok());
        }
    }

    #[test]
    fn ui_truncation_keeps_the_shape() {
        let big = json!({
            "text": "z".repeat(2000),
            "items": (0..50).collect::<Vec<i32>>(),
        });
        let out = truncate_for_ui(&big);
        assert_eq!(out["items"].as_array().unwrap().len(), 30);
        assert_eq!(out["text"].as_str().unwrap().chars().count(), 801);
    }
}
