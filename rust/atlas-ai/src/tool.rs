//! The tool-call protocol: what the model asked for, and whether it may run.
//!
//! # Two shapes, one gate
//!
//! llama.cpp delivers a tool call in one of two ways:
//!
//! * **native** — `choices[0].message.tool_calls[]`, each with a `function.name`
//!   and a `function.arguments` string;
//! * **JSON in text** — with a grammar (GBNF) the model is *forced* to emit the
//!   call as JSON inside `content`: `{"name": "search_news", "arguments": {…}}`,
//!   sometimes wrapped in `{"tool_calls": […]}` or an array, sometimes fenced with
//!   ```` ```json ````.
//!
//! Both are parsed into [`PendingCall`] — a name plus the **unparsed** argument
//! text — and both go through the same gate: [`ToolRegistry::prepare`], which
//! looks the name up and validates the arguments against that tool's schema.
//! Nothing else in this crate runs a tool.
//!
//! # Malformed is a turn, not a crash
//!
//! An unknown tool, arguments that are not a JSON object, an argument of the wrong
//! type, a missing required argument: each becomes a `String` reason that the loop
//! sends back as the tool result. The model gets to fix it. What must never happen
//! is the other thing — executing half a call, or coercing `"1; DROP TABLE"` into a
//! number by accident. So the coercions are the *documented* ones only: an integer
//! argument keeps its digits and is clamped to the schema's `minimum`/`maximum`, a
//! boolean accepts `1/true/yes`, a string is cleaned by
//! [`atlas_guard::cleantext::clean`] and capped, an array is truncated — and an
//! argument the schema does not name is dropped, because every ATLAS schema is
//! closed (`additionalProperties: false`).
//!
//! # The three numbers that bound a hostile answer
//!
//! A model that reads attacker-controlled pages is a channel for those pages
//! (`docs/THREAT-MODEL.md` §1), so its answer is treated as three separate
//! quantities, each with one cap and one sentence:
//!
//! | Quantity | Cap | What it stops |
//! | --- | --- | --- |
//! | calls in one turn | [`MAX_CALLS_PER_TURN`] = 8 | one answer becoming a thousand handler calls |
//! | bytes of argument text | [`MAX_ARGUMENT_BYTES`] = 400 000 | a 1 MB parse per call, and a 1 MB frame |
//! | characters of a shown name | [`MAX_NAME_SHOWN`] = 60 | a 10 kB name becoming a 10 kB (or 50 kB) sentence |
//!
//! Each cap is *above* whatever a legitimate answer can need — the longest shipped
//! tool name is 14 characters, the widest shipped schema is ≈ 335 kB, and a real
//! research turn asks for one to four tools — so none of them removes a feature.
//! They only decide how a hostile answer ends: with one sentence, not with work.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::chat::{ChatReply, RawCall};
use crate::context::{MAX_ARG_CHARS, MAX_ARG_ITEM, MAX_ARG_ITEMS};

/// How many arguments [`argument_help`] names. `save_article` declares ten; a cap
/// keeps a refusal short if a schema ever grows past what a model can read back.
const HELP_ARGS: usize = 12;

/// How many tool calls one model turn may contain.
///
/// This is a cap on *work the model can ask for in one answer*, and it exists
/// because nothing else in the loop bounds it: `MAX_STEPS` counts model turns, not
/// calls, so a single turn carrying a thousand calls used to run a thousand
/// handlers — each of them a real write, fetch or browser navigation — inside one
/// step. Measured before this constant existed: a 51 891-byte reply of 1 000
/// `search_news` calls made 1 000 handler calls, 2 003 events and took 12.9 s in a
/// debug build with a handler that only increments a counter. With the shipped
/// handlers that is 1 000 database writes or 1 000 gated fetches from one hostile
/// answer, which is the tool-authority escalation the threat model says cannot
/// happen (§4.12).
///
/// Eight is not a taste: `MAX_STEPS` is 10, so the run can make at most 80 calls
/// either way, and a real research turn asks for one to four. Calls past the cap
/// are *counted, not built* — the model is told in one sentence
/// ([`crate::agent::calls_refused_sentence`]) and may ask for the rest one
/// at a time, so no feature is lost to the cap.
///
/// The cap is applied twice, at the two places a list of calls is built: here, on a
/// finished reply ([`turn_of_counted`], which counts what it refused), and in
/// [`crate::chat::Aggregator`], which refuses a *stream slot* past it — a stream can
/// name as many `tool_calls[].index` slots as it likes, and the second place is
/// where that memory is decided.
pub const MAX_CALLS_PER_TURN: usize = 8;

/// How many characters of a *model-written* tool name are ever shown back — in a
/// refusal, in an event, or in a sentence to the model.
///
/// A tool name is model output, and model output is unbounded: measured, a 10 kB
/// name produced a 10 045-byte `unknown tool` refusal, and a name of 10 000 control
/// characters produced 50 045 bytes (5× growth, because `{:?}` escapes each one).
/// That sentence is prompt, and the same name is an SSE frame, so both are capped
/// here at a length no real schema name reaches (the longest shipped name is 14
/// characters). Nothing is repaired: the name is looked up as written, and only the
/// copy that is *shown* is shortened.
pub const MAX_NAME_SHOWN: usize = 60;

/// How many bytes of model-written argument text this crate will parse at all.
///
/// A schema bounds each argument, so it bounds the whole call too: the widest
/// shipped schema is `save_article` (8 string arguments × [`MAX_ARG_CHARS`] 40 000
/// plus 2 arrays × 32 × [`MAX_ARG_ITEM`] 200, ≈ 335 kB once JSON quoting is
/// counted). Ten times one argument — 400 kB — is therefore above anything a
/// legitimate call can be, and below the 8 MB a whole reply may be. Over the cap
/// the answer is one sentence instead of a parse: measured, a 1 MB argument string
/// cost 37 ms of `serde_json` before it was thrown away by the string cap.
pub const MAX_ARGUMENT_BYTES: usize = 400_000;

/// The copy of a tool name that goes into a prompt or a frame, capped and stripped
/// of control characters. One rule, so the refusal in [`ToolRegistry::prepare`], the
/// event in [`crate::agent::Event::ToolResult`] and the no-tools sentence in
/// [`crate::agent::no_tools_sentence`] cannot disagree about how much of a name is shown.
pub fn shown_name(name: &str) -> String {
    crate::prompts::cut_chars(name, MAX_NAME_SHOWN)
}

// ---------------------------------------------------------------------------
// schemas
// ---------------------------------------------------------------------------

/// One tool: the name the model must use, the description it reads, and a JSON
/// Schema (draft-07 subset) for the arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// The OpenAI-shaped entry llama.cpp expects in `tools`.
    pub fn wire(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        })
    }

    /// The declared properties, if the schema is an object.
    pub fn properties(&self) -> Map<String, Value> {
        self.parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default()
    }

    /// The required argument names.
    pub fn required(&self) -> Vec<String> {
        self.parameters
            .get("required")
            .and_then(|r| r.as_array())
            .map(|array| {
                array
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

/// What the loop can do with a validated call. Implemented outside this crate —
/// by whoever owns the database, the graph, the browser. This crate never
/// implements one: it drives the loop, it does not act.
pub trait ToolHandler: Send + Sync {
    /// `args` is already validated against the schema. `Err` is a tool failure the
    /// model gets told about; it is never fatal to the loop.
    fn call(&self, args: &Map<String, Value>) -> Result<Value, String>;
}

/// Name → description → schema → handler.
#[derive(Default)]
pub struct ToolRegistry {
    specs: Vec<ToolSpec>,
    handlers: HashMap<String, Box<dyn ToolHandler>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field(
                "tools",
                &self.specs.iter().map(|s| s.name.as_str()).collect::<Vec<&str>>(),
            )
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> ToolRegistry {
        ToolRegistry::default()
    }

    /// Register a tool and the handler that runs it.
    pub fn register(&mut self, spec: ToolSpec, handler: Box<dyn ToolHandler>) -> &mut ToolRegistry {
        self.handlers.insert(spec.name.clone(), handler);
        self.push_spec(spec);
        self
    }

    /// Register a schema whose handler lands later (the caller wants the model to
    /// see the tool before the code behind it exists).
    pub fn register_spec(&mut self, spec: ToolSpec) -> &mut ToolRegistry {
        self.push_spec(spec);
        self
    }

    fn push_spec(&mut self, spec: ToolSpec) {
        if let Some(existing) = self.specs.iter_mut().find(|s| s.name == spec.name) {
            *existing = spec;
        } else {
            self.specs.push(spec);
        }
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    pub fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.specs.iter().find(|spec| spec.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.specs.iter().map(|s| s.name.clone()).collect();
        names.sort();
        names
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// The array to put in `tools` on the request. Empty registry → empty array.
    pub fn wire(&self) -> Value {
        Value::Array(self.specs.iter().map(ToolSpec::wire).collect())
    }

    /// Run one validated call. An unknown name is a refusal, never a panic.
    pub fn call(&self, name: &str, args: &Map<String, Value>) -> Result<Value, String> {
        match self.handlers.get(name) {
            Some(handler) => handler.call(args),
            None => Err(format!("unknown tool: {}", name)),
        }
    }

    /// The gate: name lookup plus schema validation.
    ///
    /// Returns the typed call, or the reason the model may retry with. An unknown
    /// tool lists the tools that do exist, because that is what a small model
    /// needs to correct itself; with an empty registry it says so instead of
    /// ending the sentence in nothing.
    pub fn prepare(&self, pending: &PendingCall) -> Result<ToolCall, String> {
        let spec = match self.spec(pending.name()) {
            Some(spec) => spec,
            None => {
                // The name is model text: look it up as written, show it capped
                // ([`MAX_NAME_SHOWN`]), because this sentence becomes prompt and a
                // 10 kB name used to become a 10 kB refusal.
                let shown = shown_name(pending.name());
                let names = self.names();
                return Err(if names.is_empty() {
                    format!(
                        "unknown tool {:?}, and no tools are available in this run - answer in one \
                         line from what you already know and say what is missing",
                        shown
                    )
                } else {
                    format!(
                        "unknown tool {:?}. Available tools: {}",
                        shown,
                        names.join(", ")
                    )
                });
            }
        };
        let args = coerce_args(spec, pending.arguments())?;
        Ok(ToolCall {
            id: pending.id.clone(),
            name: spec.name.clone(),
            args,
            source: pending.source,
        })
    }
}

// ---------------------------------------------------------------------------
// what the model produced
// ---------------------------------------------------------------------------

/// Where a call came from — useful in a server log and for one test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSource {
    /// `message.tool_calls[]`.
    Native,
    /// A JSON object or array inside `content`.
    JsonInText,
    /// The tag form inside `content` — `<function=name>` with `<parameter=key>`
    /// blocks. Qwen's chat template produces this shape, and a model that has just
    /// run out of tool turns writes it as prose because it still wants to call
    /// something. Measured on the shipped model: a research pass whose step budget
    /// ended answered with exactly this block and a run that had read the article
    /// stored nothing, because nothing parsed it.
    MarkupInText,
}

/// One call as the model wrote it: a name and argument **text**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub id: String,
    pub name: String,
    arguments: String,
    pub source: CallSource,
}

impl PendingCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: impl Into<String>, source: CallSource) -> PendingCall {
        PendingCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
            source,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The argument text as the model produced it (never rewritten).
    pub fn arguments(&self) -> &str {
        &self.arguments
    }

    /// The identity of this call for the loop's repeat detector: the name, plus a
    /// digest and a length of the canonicalised arguments. Two calls with the same
    /// name and the same arguments are the same call, however the model spelled the
    /// whitespace.
    ///
    /// A digest rather than the arguments themselves: the brake keeps
    /// [`crate::agent::REPEAT_WINDOW`] steps of these keys for the whole run, and the
    /// text is the model's — measured, eight 1 MiB argument lists became 8.4 MB of
    /// key to answer one question, "have I seen this call?". A hash collision would
    /// hide a real call; the length rides along so a collision has to be the same
    /// size as well, and 64 bits of a canonical form is far more than a detector
    /// needs. Only identity, never a security decision.
    pub fn key(&self) -> String {
        let canonical = match serde_json::from_str::<Value>(&self.arguments) {
            Ok(value) => canonical_json(&value),
            Err(_) => self.arguments.trim().to_string(),
        };
        let (digest, length) = digest_of(&canonical);
        format!("{}\u{1}{:016x}{}", self.name, digest, length)
    }

    /// The wire form an assistant message carries, so the tool reply can be
    /// addressed to this call id.
    pub fn to_wire(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "type": "function",
            "function": { "name": self.name, "arguments": self.arguments },
        })
    }
}

/// A validated call: the name exists and every argument matches its schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Map<String, Value>,
    pub source: CallSource,
}

impl ToolCall {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "args": Value::Object(self.args.clone()),
            "source": match self.source {
                CallSource::Native => "native",
                CallSource::JsonInText => "json-in-text",
                CallSource::MarkupInText => "markup-in-text",
            },
        })
    }
}

/// One model turn: either it answered, or it asked for tools.
#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    Answer(String),
    Calls(Vec<PendingCall>),
}

/// Read one reply as a turn.
///
/// Native calls win. Only when there are none is `content` examined for the
/// JSON-in-text form — so a model that answers with a JSON *verdict* is not
/// mistaken for one that is calling a tool, and a model that calls a tool without
/// a grammar is still understood.
///
/// At most [`MAX_CALLS_PER_TURN`] calls survive. A caller that needs to tell the
/// model how many it refused asks [`turn_of_counted`]; this entry point keeps the
/// old path (one turn in, one turn out) and is capped all the same, so no caller can
/// get an unbounded call list by accident.
pub fn turn_of(reply: &ChatReply) -> Turn {
    turn_of_counted(reply).0
}

/// [`turn_of`], plus how many calls the cap refused.
///
/// The count is what lets the loop answer a hostile or confused turn with one
/// sentence instead of running a thousand tools. `refused` is 0 for a normal turn,
/// so a caller that ignores it behaves exactly as before.
pub fn turn_of_counted(reply: &ChatReply) -> (Turn, usize) {
    if !reply.calls.is_empty() {
        let refused = reply.calls.len().saturating_sub(MAX_CALLS_PER_TURN);
        let calls = reply
            .calls
            .iter()
            .take(MAX_CALLS_PER_TURN)
            .enumerate()
            .map(|(index, raw)| native_call(raw, index))
            .collect();
        return (Turn::Calls(calls), refused);
    }
    match calls_in_text_counted(&reply.content) {
        Some((calls, refused)) => (Turn::Calls(calls), refused),
        None => (Turn::Answer(reply.content.clone()), 0),
    }
}

fn native_call(raw: &RawCall, index: usize) -> PendingCall {
    let id = if raw.id.trim().is_empty() {
        format!("call_{}", index)
    } else {
        raw.id.clone()
    };
    PendingCall::new(id, raw.name.clone(), raw.arguments.clone(), CallSource::Native)
}

/// The JSON-in-text form, if `content` is one.
pub fn calls_in_text(content: &str) -> Option<Vec<PendingCall>> {
    calls_in_text_counted(content).map(|(calls, _)| calls)
}

/// [`calls_in_text`], plus how many calls [`MAX_CALLS_PER_TURN`] refused.
///
/// The cap is applied while the list is *built* rather than after, so a reply with
/// a hundred thousand call objects allocates eight [`PendingCall`]s and counts the
/// rest — a 1 MB array is parsed once, not turned into 100 000 structs.
pub fn calls_in_text_counted(content: &str) -> Option<(Vec<PendingCall>, usize)> {
    if let Some(value) = parse_json_loose(content) {
        if let Some((calls, refused)) = calls_from_value(&value) {
            if !calls.is_empty() {
                return Some((calls, refused));
            }
        }
    }
    // Not JSON: the tag form, which is what a Qwen template emits and what the
    // shipped model writes when it has no tool turn left. Tried second, never
    // first, so a JSON call is never reinterpreted by the looser grammar.
    let calls = calls_in_markup(content);
    if calls.is_empty() {
        None
    } else {
        Some((calls, 0))
    }
}

/// The tag form: `<function=name>` … `<parameter=key>` value `</parameter>` … .
///
/// Qwen's template spells a call like this, and a model that has run out of tool
/// turns writes it as prose. Measured on this machine, a research pass that had
/// read its article ended with
///
/// ```text
/// <tool_call>
/// <function=save_article>
/// <parameter=url>
/// https://…
/// </parameter>
/// <parameter>summary>
/// TSMC and Samsung paused shipments…
/// </parameter>
/// </function>
/// </tool_call>
/// ```
///
/// — note the second key: the model wrote `<parameter>summary>` instead of
/// `<parameter=summary>`, which is why the key is read as "everything after
/// `<parameter` up to the next `>` with `=` and `>` skipped", not as "everything
/// after `=`". A key with no value, an empty name, or a block with no parameters is
/// dropped rather than repaired: the loop answers a dropped call with a sentence the
/// model can act on, and a repaired one with a call it did not write.
///
/// The values become a JSON object, so the same `coerce_args` validation decides
/// whether the call is legal. Numbers and booleans stay strings here and are
/// converted by that pass — this function never guesses a type from text.
fn calls_in_markup(content: &str) -> Vec<PendingCall> {
    let mut calls: Vec<PendingCall> = Vec::new();
    let mut rest = content;
    while calls.len() < MAX_CALLS_PER_TURN {
        let Some(function_at) = rest.find("<function") else {
            break;
        };
        rest = &rest[function_at + "<function".len()..];
        let Some((name, after_name)) = tag_head(rest) else {
            continue;
        };
        // The body of the call ends at `</function>`, or at the end of the text when
        // the model never closed it (a cut-off answer).
        let (body, tail) = match after_name.find("</function>") {
            Some(close) => (&after_name[..close], &after_name[close + "</function>".len()..]),
            None => (after_name, ""),
        };
        rest = tail;
        let arguments = markup_arguments(body);
        if name.is_empty() {
            continue;
        }
        calls.push(PendingCall::new(
            format!("call_markup_{}", calls.len()),
            name,
            arguments.to_string(),
            CallSource::MarkupInText,
        ));
    }
    calls
}

/// The name after `<function` and the text that follows its closing `>`.
fn tag_head(text: &str) -> Option<(String, &str)> {
    let close = text.find('>')?;
    let head = text[..close].trim_start_matches(['=', ' ']);
    let name: String = head
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();
    Some((name, &text[close + 1..]))
}

/// The `<parameter…>` blocks of one call body, as a JSON object.
fn markup_arguments(body: &str) -> Value {
    let mut map = serde_json::Map::new();
    let mut rest = body;
    while let Some(at) = rest.find("<parameter") {
        rest = &rest[at + "<parameter".len()..];
        let Some(first) = rest.find('>') else {
            break;
        };
        // Two spellings, both measured: `<parameter=url>` and the model's slip
        // `<parameter>summary>`. The key is the text up to the closing `>`; when the
        // first `>` closes nothing (that slip), the key is the word after it.
        let inline = rest[..first].trim_start_matches(['=', '>', ' ']).trim().to_string();
        let (key, body_at) = if inline.is_empty() {
            let after_first = &rest[first + 1..];
            match after_first.find('>') {
                Some(second) => (after_first[..second].trim().to_string(), first + 1 + second + 1),
                None => break,
            }
        } else {
            (inline, first + 1)
        };
        let after = &rest[body_at..];
        let (value, tail) = match after.find("</parameter>") {
            Some(end) => (&after[..end], &after[end + "</parameter>".len()..]),
            None => (after, ""),
        };
        rest = tail;
        // A key that is not a name is not a parameter; a key without a value is a
        // parameter the model did not fill in, and the schema decides about that.
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        map.insert(key, Value::String(value.trim().to_string()));
    }
    Value::Object(map)
}

/// Interpret a parsed JSON value as zero or more calls, and say how many the cap
/// refused. Every entry of the array must be a call object: a `[call, 42]` is not a
/// call list, exactly as before — the cap only decides how far the check looks.
fn calls_from_value(value: &Value) -> Option<(Vec<PendingCall>, usize)> {
    match value {
        Value::Array(items) => Some(limited(items.iter().enumerate())),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("tool_calls") {
                return Some(limited(items.iter().enumerate()));
            }
            // A single bare call.
            call_from_object(value, 0).map(|call| (vec![call], 0))
        }
        _ => None,
    }
}

/// The first [`MAX_CALLS_PER_TURN`] call objects of an array, and how many were
/// left. `None` from [`call_from_object`] aborts, because then the value is not a
/// call list at all.
fn limited<'a>(items: impl Iterator<Item = (usize, &'a Value)>) -> (Vec<PendingCall>, usize) {
    let mut calls: Vec<PendingCall> = Vec::new();
    let mut refused = 0usize;
    for (index, item) in items {
        if calls.len() >= MAX_CALLS_PER_TURN {
            refused += 1;
            continue;
        }
        match call_from_object(item, index) {
            Some(call) => calls.push(call),
            // Not a call list: `limited` cannot say "no" through its return type, so
            // it says "nothing" — the same answer `?` gave before the count existed.
            None => return (Vec::new(), 0),
        }
    }
    (calls, refused)
}

/// One call object → [`PendingCall`]. `None` when there is no usable name.
///
/// Accepted spellings, all of which real grammars produce:
/// `{"name", "arguments"}`, `{"name", "parameters"}`, `{"tool", "arguments"}`,
/// `{"function": {"name", "arguments"}}`. Arguments may be an object (re-serialised)
/// or a string (kept as-is, and validated later — a string that is not JSON is the
/// model's mistake to fix, not ours to repair).
fn call_from_object(value: &Value, index: usize) -> Option<PendingCall> {
    let map = value.as_object()?;
    let function = map.get("function").and_then(|f| f.as_object());
    let name = map
        .get("name")
        .and_then(|n| n.as_str())
        .or_else(|| map.get("tool").and_then(|n| n.as_str()))
        .or_else(|| function.and_then(|f| f.get("name")).and_then(|n| n.as_str()))?;
    if name.trim().is_empty() {
        return None;
    }
    let arguments = map
        .get("arguments")
        .or_else(|| map.get("parameters"))
        .or_else(|| function.and_then(|f| f.get("arguments")))
        .or_else(|| function.and_then(|f| f.get("parameters")))
        .map(argument_text_of)
        .unwrap_or_else(|| "{}".to_string());
    let id = map
        .get("id")
        .and_then(|i| i.as_str())
        .filter(|i| !i.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_text_{}", index));
    Some(PendingCall::new(id, name.trim(), arguments, CallSource::JsonInText))
}

fn argument_text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "{}".to_string(),
        other => other.to_string(),
    }
}

/// Robust JSON out of model output: fences stripped, earliest `{` or `[` tried.
///
/// `ai.py::parse_json_loose`, kept as tolerant as it was and no more: it returns
/// the first value that decodes, and `None` when nothing does.
pub fn parse_json_loose(raw: &str) -> Option<Value> {
    if raw.trim().is_empty() {
        return None;
    }
    let mut text = raw.trim().to_string();
    if text.starts_with("```") {
        // Drop the opening fence line, with or without a language tag.
        match text.find('\n') {
            Some(pos) => text = text[pos + 1..].to_string(),
            None => text = text.trim_start_matches('`').to_string(),
        }
        if let Some(pos) = text.rfind("```") {
            if text[pos..].trim_end() == "```" {
                text = text[..pos].to_string();
            }
        }
        text = text.trim().to_string();
    }
    let mut starts: Vec<usize> = Vec::new();
    if let Some(pos) = text.find('{') {
        starts.push(pos);
    }
    if let Some(pos) = text.find('[') {
        starts.push(pos);
    }
    starts.sort_unstable();
    for start in starts {
        let candidate = &text[start..];
        let mut stream = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            return Some(value);
        }
    }
    None
}

/// The identity of a piece of model-written text: a 64-bit digest and its length.
///
/// Both halves are wanted. The digest makes the key's size independent of the
/// text's, which is what a brake that remembers [`crate::agent::REPEAT_WINDOW`]
/// steps needs; the length makes an accidental collision have to be the same size
/// too. `DefaultHasher` is `SipHash` with fixed keys, so the same text gives the same
/// answer in the same process — a cache/detector identity, never a security digest.
pub(crate) fn digest_of(text: &str) -> (u64, usize) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    (hasher.finish(), text.len())
}

/// Key-sorted JSON, so two spellings of the same arguments compare equal.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = String::from("{");
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                out.push_str(&canonical_json(&map[*key]));
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let mut out = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_json(item));
            }
            out.push(']');
            out
        }
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// the coercion gate
// ---------------------------------------------------------------------------

/// The arguments one tool accepts, in the words the model needs to retry.
///
/// Derived from the schema — name, type, bounds, whether it is required — so a
/// refusal that names this tool can also say what to send instead. At most
/// [`HELP_ARGS`] arguments are listed: every ATLAS schema is smaller than that, and
/// a refusal that is longer than the request is a refusal the model has to parse.
fn argument_help(spec: &ToolSpec) -> String {
    let required = spec.required();
    let mut parts: Vec<String> = Vec::new();
    for (name, property) in spec.properties().iter().take(HELP_ARGS) {
        let kind = property.get("type").and_then(|t| t.as_str()).unwrap_or("value");
        let mut piece = format!("{}: {}", name, kind);
        if kind == "integer" {
            let low = property.get("minimum").and_then(|m| m.as_i64());
            let high = property.get("maximum").and_then(|m| m.as_i64());
            if let (Some(low), Some(high)) = (low, high) {
                piece.push_str(&format!(" {}..{}", low, high));
            }
        }
        if required.iter().any(|r| r == name) {
            piece.push_str(", required");
        }
        parts.push(piece);
    }
    if parts.is_empty() {
        return format!("{} takes no arguments", spec.name);
    }
    format!("{} accepts {{{}}}", spec.name, parts.join("; "))
}

/// Validate model-supplied arguments against a tool schema.
///
/// The rules, and nothing beyond them:
///
/// * an argument the schema does not declare is **dropped** (every ATLAS schema is
///   closed, so an invented argument is a mistake, not a feature);
/// * `integer`: digits of a string (as `ai.py` did — `"15 results"` → `15`), a
///   number truncated, a boolean as 0/1; then clamped to `minimum`/`maximum`;
/// * `boolean`: a real boolean, or `1` / `true` / `yes` (case-insensitive);
/// * `string`: everything JSON-shaped turned into text, cleaned by
///   [`atlas_guard::cleantext::clean`] (control and bidi characters out) and capped
///   at [`MAX_ARG_CHARS`];
/// * `array`: a single value becomes a one-item array, at most [`MAX_ARG_ITEMS`]
///   items, each cleaned and capped at [`MAX_ARG_ITEM`];
/// * anything else is passed through untouched (the model's JSON is the schema's
///   business, not this function's);
/// * argument text over [`MAX_ARGUMENT_BYTES`] is refused before it is parsed: a
///   schema bounds every argument, so it bounds the call, and a body past that
///   cannot be one;
/// * a required argument that ends up missing, empty or an empty array is an
///   error naming it.
///
/// Every refusal names the tool and the arguments it accepts
/// (`argument_help`): "missing required argument(s): query" tells a model what it
/// got wrong but not what to send, and a model that has to guess retries blind.
pub fn coerce_args(spec: &ToolSpec, raw: &str) -> Result<Map<String, Value>, String> {
    let text = raw.trim();
    // Before the parser sees it: an argument body larger than any schema could
    // describe is not a call, it is a parse the model asked for. One sentence is
    // cheaper and tells the model what to do, where a 1 MB `serde_json` pass (37 ms
    // measured) told it nothing.
    if text.len() > MAX_ARGUMENT_BYTES {
        return Err(format!(
            "the arguments were {} bytes and the limit is {} - call {} again with just the \
             arguments its schema names; {}",
            text.len(),
            MAX_ARGUMENT_BYTES,
            spec.name,
            argument_help(spec)
        ));
    }
    let given: Value = if text.is_empty() {
        Value::Object(Map::new())
    } else {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => value,
            Err(_) => {
                return Err(format!(
                    "arguments were not valid JSON - call {} again with a JSON object; {}",
                    spec.name,
                    argument_help(spec)
                ))
            }
        }
    };
    let given = match given {
        Value::Object(map) => map,
        Value::Array(_) => {
            return Err(format!(
                "arguments must be a JSON object, not an array - call {} with one object; {}",
                spec.name,
                argument_help(spec)
            ))
        }
        _ => {
            return Err(format!(
                "arguments must be a JSON object - call {} with one object; {}",
                spec.name,
                argument_help(spec)
            ))
        }
    };
    let properties = spec.properties();
    let mut args: Map<String, Value> = Map::new();
    for (key, value) in given {
        let property = match properties.get(&key) {
            Some(property) => property,
            None => continue, // an invented argument never reaches a handler
        };
        match coerce_one(property, &value) {
            Some(coerced) => {
                args.insert(key, coerced);
            }
            None => continue,
        }
    }
    let mut missing: Vec<String> = Vec::new();
    for name in spec.required() {
        let empty = match args.get(&name) {
            None | Some(Value::Null) => true,
            Some(Value::String(text)) => text.is_empty(),
            Some(Value::Array(items)) => items.is_empty(),
            Some(_) => false,
        };
        if empty {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "missing required argument(s) for {}: {}. {}",
            spec.name,
            missing.join(", "),
            argument_help(spec)
        ));
    }
    Ok(args)
}

/// One argument against one property schema. `None` means "drop this argument".
fn coerce_one(property: &Value, value: &Value) -> Option<Value> {
    let kind = property.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match kind {
        "integer" | "number" => {
            let number = match value {
                Value::String(text) => {
                    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
                    if digits.is_empty() {
                        return None;
                    }
                    digits.parse::<i64>().ok()?
                }
                Value::Number(number) => number.as_i64().or_else(|| {
                    number.as_f64().map(|f| f.trunc() as i64)
                })?,
                Value::Bool(flag) => {
                    if *flag {
                        1
                    } else {
                        0
                    }
                }
                _ => return None,
            };
            let low = property
                .get("minimum")
                .and_then(|m| m.as_i64())
                .unwrap_or(-1_000_000_000);
            let high = property
                .get("maximum")
                .and_then(|m| m.as_i64())
                .unwrap_or(1_000_000_000);
            let clamped = number.max(low).min(high);
            if kind == "number" {
                Some(Value::from(clamped))
            } else {
                Some(Value::from(clamped))
            }
        }
        "boolean" => {
            let flag = match value {
                Value::Bool(flag) => *flag,
                other => {
                    let text = scalar_text(other).trim().to_ascii_lowercase();
                    text == "1" || text == "true" || text == "yes"
                }
            };
            Some(Value::Bool(flag))
        }
        "string" => {
            let text = match value {
                Value::Null => String::new(),
                other => scalar_text(other),
            };
            let cleaned = atlas_guard::cleantext::clean(&text, Some(MAX_ARG_CHARS)).text;
            Some(Value::String(cleaned))
        }
        "array" => {
            let items: Vec<&Value> = match value {
                Value::Array(items) => items.iter().take(MAX_ARG_ITEMS).collect(),
                other => vec![other],
            };
            // An array whose schema says `items: {type: object}` is a list of little
            // records (`save_article`'s `entities`: a name *and* the kind the model
            // decided). Flattening one through `scalar_text` would hand the handler a
            // JSON string where the schema promised two fields - and the second answer,
            // the only one a name cannot carry itself, would be lost. Each declared field
            // is cleaned here; an undeclared one is dropped, exactly as for a top-level
            // argument.
            if let Some((fields, required)) = object_fields(property) {
                let mut out: Vec<Value> = Vec::with_capacity(items.len());
                for item in items {
                    if let Some(record) = coerce_record(&fields, &required, item) {
                        out.push(record);
                    }
                }
                return Some(Value::Array(out));
            }
            let mut out: Vec<Value> = Vec::with_capacity(items.len());
            for item in items {
                let text = scalar_text(item);
                let cleaned = atlas_guard::cleantext::clean(&text, Some(MAX_ARG_ITEM)).text;
                out.push(Value::String(cleaned));
            }
            Some(Value::Array(out))
        }
        _ => Some(value.clone()),
    }
}

/// The declared fields of an object schema, when a property's items are records.
///
/// `None` for every ordinary array (items are strings or nothing was declared), which is
/// what keeps the string path above the one most tools take.
fn object_fields(property: &Value) -> Option<(Map<String, Value>, Vec<String>)> {
    let items = property.get("items")?;
    if items.get("type").and_then(|kind| kind.as_str()) != Some("object") {
        return None;
    }
    let fields = match items.get("properties") {
        Some(Value::Object(fields)) => fields.clone(),
        _ => return None,
    };
    let required = items
        .get("required")
        .and_then(|list| list.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some((fields, required))
}

/// One record of an `items: {type: object}` array: the declared fields, cleaned like every
/// other string argument, and `None` when the item is not an object at all.
fn coerce_record(
    fields: &Map<String, Value>,
    required: &[String],
    item: &Value,
) -> Option<Value> {
    let given = match item {
        Value::Object(map) => map,
        _ => return None,
    };
    let mut record = Map::new();
    for (name, field) in fields {
        if let Some(value) = given.get(name) {
            if let Some(coerced) = coerce_one(field, value) {
                record.insert(name.clone(), coerced);
            }
        }
    }
    // The record's own required set, held to the same rule as a top-level argument: an
    // empty string is missing. A nameless entity is not an entity - dropping it here means
    // a handler never has to defend against one.
    let incomplete = required.iter().any(|name| match record.get(name) {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        Some(_) => false,
    });
    if record.is_empty() || incomplete {
        None
    } else {
        Some(Value::Object(record))
    }
}

/// `str(value)` as Python wrote it: a string stays itself, everything else is its
/// JSON text (a number is `3`, an object is `{"a":1}`).
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search_spec() -> ToolSpec {
        crate::specs::tools()
            .into_iter()
            .find(|spec| spec.name == "search_news")
            .expect("search_news is a real tool")
    }

    struct Echo;

    impl ToolHandler for Echo {
        fn call(&self, args: &Map<String, Value>) -> Result<Value, String> {
            Ok(json!({ "ok": true, "seen": Value::Object(args.clone()) }))
        }
    }

    fn reply_with_calls(calls: Vec<RawCall>) -> ChatReply {
        ChatReply {
            thinking: String::new(),
            model: "fake".to_string(),
            content: String::new(),
            calls,
            finish_reason: "tool_calls".to_string(),
            usage_tokens: None,
            truncated: false,
        }
    }

    fn reply_with_text(text: &str) -> ChatReply {
        ChatReply {
            thinking: String::new(),
            model: "fake".to_string(),
            content: text.to_string(),
            ..ChatReply::default()
        }
    }

    // ---- parsing ---------------------------------------------------------

    /// The block the shipped model actually wrote when its step budget ended (copied
    /// from a real research run, including the broken `<parameter>summary>` tag).
    const MEASURED_MARKUP: &str = "<tool_call>\n<function=save_article>\n<parameter=url>\n\
https://theopenpress.in/article/us-chip-curbs-tsmc-samsung-pause-china-shipments\n</parameter>\n\
<parameter=title>\nUS Chip Curbs: TSMC, Samsung Halt Shipments to China\n</parameter>\n\
<parameter>summary>\nTSMC and Samsung paused shipments of certain chips to Chinese customers.\n</parameter>\n\
<parameter=tags>\ntsmc, samsung, export controls\n</parameter>\n</function>\n</tool_call>";

    #[test]
    fn the_tag_form_the_model_writes_is_a_call() {
        let calls = calls_in_text(MEASURED_MARKUP).expect("the measured block is a call");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name(), "save_article");
        assert_eq!(calls[0].source, CallSource::MarkupInText);
        let args: Value = serde_json::from_str(calls[0].arguments()).expect("arguments are JSON");
        assert_eq!(
            args["url"],
            json!("https://theopenpress.in/article/us-chip-curbs-tsmc-samsung-pause-china-shipments")
        );
        assert_eq!(
            args["summary"],
            json!("TSMC and Samsung paused shipments of certain chips to Chinese customers."),
            "the model's `<parameter>summary>` typo still names the parameter"
        );
        assert_eq!(args["tags"], json!("tsmc, samsung, export controls"));
    }

    #[test]
    fn a_tag_form_turn_is_a_call_and_not_an_answer() {
        // The loop reads a turn through `turn_of`: if the tag form were an answer, the
        // loop would end silently — which is what a research pass did before this
        // parser existed (it had read the page and stored nothing).
        let (turn, refused) = turn_of_counted(&reply_with_text(MEASURED_MARKUP));
        assert_eq!(refused, 0);
        match turn {
            Turn::Calls(calls) => assert_eq!(calls[0].name(), "save_article"),
            other => panic!("expected calls, got {other:?}"),
        }
    }

    #[test]
    fn markup_without_a_name_is_not_a_call_and_a_parameterless_one_is() {
        assert!(calls_in_text("<tool_call>\n<function=>\n</function>").is_none());
        assert!(calls_in_text("<function>\n</function>").is_none());
        // A page that mentions a parameter outside any function block is not a call.
        assert!(calls_in_text("the page says <parameter=url> in a code sample").is_none());
        // A call with no parameters *is* a call — `stats` takes none — and the schema is
        // what decides whether a particular tool may go without arguments.
        let empty = calls_in_text("<function=stats>\n</function>").expect("a parameterless call");
        assert_eq!(empty[0].name(), "stats");
        assert_eq!(empty[0].arguments(), "{}");
    }

    #[test]
    fn two_calls_in_one_turn_are_both_read_and_capped() {
        let two = format!("{}{}", MEASURED_MARKUP, MEASURED_MARKUP);
        let calls = calls_in_text(&two).expect("two calls");
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].id, calls[1].id, "each call has its own id");
        let many = MEASURED_MARKUP.repeat(MAX_CALLS_PER_TURN + 3);
        assert_eq!(
            calls_in_text(&many).expect("many calls").len(),
            MAX_CALLS_PER_TURN,
            "the per-turn cap holds for the tag form too"
        );
    }

    #[test]
    fn a_json_call_is_never_reread_as_markup() {
        // Order matters: JSON first, markup second. A JSON call that happens to carry
        // the words in a string must keep its JSON reading.
        let json = "{\"name\": \"stats\", \"arguments\": {\"note\": \"<function=save_article>\"}}";
        let calls = calls_in_text(json).expect("a JSON call");
        assert_eq!(calls[0].name(), "stats");
        assert_eq!(calls[0].source, CallSource::JsonInText);
    }

    #[test]
    fn native_calls_are_read() {
        let reply = reply_with_calls(vec![RawCall {
            id: "call_7".to_string(),
            name: "search_news".to_string(),
            arguments: "{\"query\":\"chips\"}".to_string(),
        }]);
        match turn_of(&reply) {
            Turn::Calls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_7");
                assert_eq!(calls[0].name(), "search_news");
                assert_eq!(calls[0].source, CallSource::Native);
            }
            other => panic!("expected calls, got {:?}", other),
        }
    }

    #[test]
    fn json_in_text_is_read_in_all_its_forms() {
        let bare = calls_in_text("{\"name\": \"search_news\", \"arguments\": {\"query\": \"chips\"}}")
            .expect("bare form");
        assert_eq!(bare[0].name(), "search_news");
        assert_eq!(bare[0].arguments(), "{\"query\":\"chips\"}");

        let wrapped = calls_in_text("{\"tool_calls\": [{\"name\": \"stats\", \"arguments\": {}}]}")
            .expect("wrapped form");
        assert_eq!(wrapped[0].name(), "stats");

        let array = calls_in_text("[{\"name\": \"stats\"}, {\"name\": \"list_tags\"}]")
            .expect("array form");
        assert_eq!(array.len(), 2);

        let aliases = calls_in_text("{\"tool\": \"stats\", \"parameters\": {}}").expect("alias form");
        assert_eq!(aliases[0].name(), "stats");

        let fenced = calls_in_text("```json\n{\"name\": \"stats\", \"arguments\": {}}\n```")
            .expect("fenced form");
        assert_eq!(fenced[0].name(), "stats");
    }

    #[test]
    fn a_plain_answer_is_not_a_call() {
        let reply = reply_with_text("Chip exports fell 4% on Tuesday [Article #12].");
        assert!(matches!(turn_of(&reply), Turn::Answer(_)));
        // A JSON verdict is an answer, not a tool call.
        assert!(calls_in_text("{\"news\": true, \"reason\": \"reporting\"}").is_none());
        assert!(calls_in_text("{\"entities\": []}").is_none());
        assert!(calls_in_text("not json at all").is_none());
    }

    #[test]
    fn the_repeat_detector_key_ignores_argument_order() {
        let a = PendingCall::new("1", "search_news", "{\"query\":\"chips\",\"limit\":5}", CallSource::Native);
        let b = PendingCall::new("2", "search_news", "{ \"limit\": 5, \"query\": \"chips\" }", CallSource::Native);
        assert_eq!(a.key(), b.key());
        let c = PendingCall::new("3", "search_news", "{\"query\":\"oil\"}", CallSource::Native);
        assert_ne!(a.key(), c.key());
    }

    // ---- the gate --------------------------------------------------------

    #[test]
    fn a_malformed_call_is_rejected_before_it_can_run() {
        let spec = search_spec();
        // Not JSON.
        let error = coerce_args(&spec, "{query: chips}").unwrap_err();
        assert!(error.contains("not valid JSON"), "{}", error);
        // Not an object.
        let error = coerce_args(&spec, "[1,2,3]").unwrap_err();
        assert!(error.contains("must be a JSON object"), "{}", error);
        // Missing the required argument.
        let error = coerce_args(&spec, "{\"limit\": 5}").unwrap_err();
        assert!(error.contains("missing required"), "{}", error);
        // The same, through the registry, with the available tools named.
        let mut registry = ToolRegistry::new();
        registry.register(spec, Box::new(Echo));
        let pending = PendingCall::new("1", "nope", "{}", CallSource::Native);
        let error = registry.prepare(&pending).unwrap_err();
        assert!(error.contains("unknown tool"), "{}", error);
        assert!(error.contains("search_news"), "{}", error);
    }

    #[test]
    fn a_refusal_names_the_tool_and_what_it_accepts() {
        // A model that is told only *what* was wrong retries blind. Every refusal
        // derived from the schema names the tool and the arguments it takes.
        let spec = search_spec();
        let missing = coerce_args(&spec, "{\"limit\": 5}").unwrap_err();
        assert!(
            missing.contains("missing required argument(s) for search_news: query"),
            "{}",
            missing
        );
        assert!(missing.contains("search_news accepts {"), "{}", missing);
        assert!(missing.contains("query: string, required"), "{}", missing);
        assert!(missing.contains("hours: integer 1..720"), "{}", missing);

        let not_json = coerce_args(&spec, "{query: chips}").unwrap_err();
        assert!(not_json.contains("call search_news again with a JSON object"), "{}", not_json);
        assert!(not_json.contains("query: string, required"), "{}", not_json);

        let list = coerce_args(&spec, "[1,2,3]").unwrap_err();
        assert!(list.contains("not an array"), "{}", list);
        assert!(list.contains("call search_news with one object"), "{}", list);

        // A tool with no arguments says so instead of printing an empty brace.
        let stats = crate::specs::tools()
            .into_iter()
            .find(|spec| spec.name == "stats")
            .unwrap();
        let help = argument_help(&stats);
        assert_eq!(help, "stats takes no arguments");
    }

    #[test]
    fn an_unknown_tool_with_no_registry_does_not_end_in_nothing() {
        // The old message was "unknown tool \"x\". Available tools: " — a sentence
        // that lists no tools and tells the model nothing to do.
        let registry = ToolRegistry::new();
        let pending = PendingCall::new("1", "search_news", "{}", CallSource::Native);
        let error = registry.prepare(&pending).unwrap_err();
        assert!(error.contains("unknown tool"), "{}", error);
        assert!(!error.trim_end().ends_with(':'), "{}", error);
        assert!(error.contains("no tools are available"), "{}", error);
        assert!(error.contains("say what is missing"), "{}", error);
    }

    #[test]
    fn numbers_booleans_and_lists_are_coerced_the_documented_way() {
        let mut registry = ToolRegistry::new();
        registry.register(search_spec(), Box::new(Echo));
        let pending = PendingCall::new(
            "1",
            "search_news",
            "{\"query\":\"chips\",\"hours\":\"48 hours\",\"limit\":\"900\"}",
            CallSource::Native,
        );
        let call = registry.prepare(&pending).unwrap();
        assert_eq!(call.args["hours"], json!(48), "digits out of a string");
        assert_eq!(call.args["limit"], json!(50), "clamped to the schema maximum");
        assert_eq!(call.args["query"], json!("chips"));

        // Booleans: a real one, a yes, a nonsense value, and an array of strings.
        let web = crate::specs::tools()
            .into_iter()
            .find(|spec| spec.name == "web_search")
            .unwrap();
        let args = coerce_args(&web, "{\"query\":\"chips\",\"news_only\":\"yes\"}").unwrap();
        assert_eq!(args["news_only"], json!(true));
        let args = coerce_args(&web, "{\"query\":\"chips\",\"news_only\":\"maybe\"}").unwrap();
        assert_eq!(args["news_only"], json!(false));
        let args = coerce_args(&web, "{\"query\":\"chips\",\"news_only\":true}").unwrap();
        assert_eq!(args["news_only"], json!(true));

        let save = crate::specs::tools()
            .into_iter()
            .find(|spec| spec.name == "save_article")
            .unwrap();
        let args = coerce_args(
            &save,
            "{\"url\":\"http://x/y\",\"title\":\"T\",\"summary\":\"S\",\"topic\":\"t\",\
             \"tags\":\"nvidia\",\"categories\":[\"Market\",2,\"Tech\"]}",
        )
        .unwrap();
        assert_eq!(args["tags"], json!(["nvidia"]), "a single value becomes a list");
        assert_eq!(args["categories"], json!(["Market", "2", "Tech"]));
    }

    #[test]
    fn invented_arguments_are_dropped() {
        let spec = search_spec();
        let args = coerce_args(&spec, "{\"query\":\"chips\",\"sql\":\"DROP TABLE articles\"}")
            .unwrap();
        assert!(args.get("sql").is_none(), "an invented argument never reaches a handler");
        assert_eq!(args["query"], json!("chips"));
    }

    #[test]
    fn an_argument_of_the_wrong_type_is_dropped_not_guessed() {
        let spec = search_spec();
        let args = coerce_args(&spec, "{\"query\":\"chips\",\"hours\":\"soon\"}").unwrap();
        assert!(args.get("hours").is_none(), "no digits means no number");
    }

    #[test]
    fn model_text_is_cleaned_and_capped() {
        let spec = search_spec();
        let hostile = format!("chips\u{202e}{}", "x".repeat(200_000));
        let args = coerce_args(&spec, &json!({ "query": hostile }).to_string()).unwrap();
        let query = args["query"].as_str().unwrap();
        assert!(!query.contains('\u{202e}'), "bidi overrides are removed");
        assert!(query.chars().count() <= MAX_ARG_CHARS);
    }

    #[test]
    fn the_registry_runs_nothing_it_does_not_know() {
        let mut registry = ToolRegistry::new();
        registry.register(search_spec(), Box::new(Echo));
        let mut args = Map::new();
        args.insert("query".to_string(), json!("chips"));
        assert!(registry.call("search_news", &args).is_ok());
        let error = registry.call("rm_rf", &args).unwrap_err();
        assert!(error.contains("unknown tool"));
        assert_eq!(registry.names(), vec!["search_news".to_string()]);
        assert_eq!(registry.wire()[0]["type"], json!("function"));
    }

    // ---- the caps that keep one hostile answer to one sentence ----------

    #[test]
    fn one_turn_cannot_ask_for_more_calls_than_the_cap() {
        // Before `MAX_CALLS_PER_TURN` the parser handed the loop every call the
        // model wrote. Measured on this machine: a 51 891-byte reply of 1 000
        // `search_news` calls became 1 000 `PendingCall`s, which the loop executed
        // as 1 000 handler calls and 2 003 events in 12.9 s — with the shipped
        // handlers, a thousand writes or gated fetches from one answer.
        let items: Vec<Value> = (0..1_000)
            .map(|index| json!({ "name": "search_news", "arguments": { "query": format!("q{}", index) } }))
            .collect();
        let in_text = reply_with_text(&Value::Array(items).to_string());
        let (turn, refused) = turn_of_counted(&in_text);
        match turn {
            Turn::Calls(calls) => {
                assert_eq!(calls.len(), MAX_CALLS_PER_TURN, "the first calls are kept");
                assert_eq!(refused, 1_000 - MAX_CALLS_PER_TURN, "the rest are counted");
                // The kept calls are the first ones, in the order the model wrote them.
                assert_eq!(calls[0].arguments(), "{\"query\":\"q0\"}");
            }
            other => panic!("expected calls, got {:?}", other),
        }

        // The native shape (`message.tool_calls[]`) is capped the same way.
        let raw: Vec<RawCall> = (0..1_000)
            .map(|index| RawCall {
                id: format!("c{}", index),
                name: "search_news".to_string(),
                arguments: format!("{{\"query\":\"q{}\"}}", index),
            })
            .collect();
        let (turn, refused) = turn_of_counted(&reply_with_calls(raw));
        match turn {
            Turn::Calls(calls) => {
                assert_eq!(calls.len(), MAX_CALLS_PER_TURN);
                assert_eq!(refused, 1_000 - MAX_CALLS_PER_TURN);
            }
            other => panic!("expected calls, got {:?}", other),
        }

        // The old entry point is capped too: no caller can get an unbounded list
        // by using the function that existed before the cap.
        let reply = reply_with_text(
            &json!([{ "name": "stats" }, { "name": "stats" }, { "name": "stats" },
                    { "name": "stats" }, { "name": "stats" }, { "name": "stats" },
                    { "name": "stats" }, { "name": "stats" }, { "name": "stats" }])
            .to_string(),
        );
        match turn_of(&reply) {
            Turn::Calls(calls) => assert_eq!(calls.len(), MAX_CALLS_PER_TURN),
            other => panic!("expected calls, got {:?}", other),
        }

        // A turn inside the cap is untouched, and refused is zero — the cap is a
        // fence, not a policy about how many tools a model may use.
        let (turn, refused) = turn_of_counted(&reply_with_text(
            "[{\"name\":\"stats\"}, {\"name\":\"list_tags\"}]",
        ));
        match turn {
            Turn::Calls(calls) => assert_eq!(calls.len(), 2),
            other => panic!("expected calls, got {:?}", other),
        }
        assert_eq!(refused, 0);
        // A not-a-call list is still not a call list, however long it is.
        let junk: Vec<Value> = (0..50).map(|index| json!(index)).collect();
        assert!(calls_in_text(&Value::Array(junk).to_string()).is_none());
    }

    #[test]
    fn a_tool_name_is_capped_wherever_it_is_shown() {
        // A name is model output. Before `MAX_NAME_SHOWN`: a 10 kB name produced a
        // 10 045-byte refusal, and 10 000 control characters produced 50 045 bytes
        // (each one is escaped as `\u{1}` by `{:?}`), and that sentence is prompt.
        let mut registry = ToolRegistry::new();
        registry.register(search_spec(), Box::new(Echo));
        let long = registry
            .prepare(&PendingCall::new("1", "a".repeat(10_000), "{}", CallSource::Native))
            .unwrap_err();
        assert!(long.contains("unknown tool"), "{}", long);
        assert!(long.len() < 300, "the refusal is {} bytes", long.len());
        assert!(long.contains(&"a".repeat(MAX_NAME_SHOWN)));
        assert!(!long.contains(&"a".repeat(MAX_NAME_SHOWN + 1)));

        let controls = registry
            .prepare(&PendingCall::new(
                "1",
                "\u{1}".repeat(10_000),
                "{}",
                CallSource::Native,
            ))
            .unwrap_err();
        assert!(controls.len() < 400, "the refusal is {} bytes", controls.len());
        // The empty registry has no names to list, so the other branch is capped too.
        let empty = ToolRegistry::new()
            .prepare(&PendingCall::new("1", "b".repeat(10_000), "{}", CallSource::Native))
            .unwrap_err();
        assert!(empty.len() < 300, "the refusal is {} bytes", empty.len());

        // One rule for every place a name is shown: the sentence a run with no tools
        // says quotes the same capped copy.
        let sentence = crate::agent::no_tools_sentence(&"c".repeat(10_000));
        assert!(sentence.len() < 300, "the sentence is {} bytes", sentence.len());
        assert_eq!(shown_name(&"d".repeat(10_000)), "d".repeat(MAX_NAME_SHOWN));
        // A real name is not touched.
        assert_eq!(shown_name("search_news"), "search_news");
    }

    #[test]
    fn argument_text_over_the_byte_cap_is_one_sentence_not_a_parse() {
        // The cap is above anything a schema can describe and below the 8 MB a
        // whole reply may be, so it can only ever refuse a body that is not a call.
        let spec = search_spec();
        let huge = format!("{{\"query\":\"{}\"}}", "x".repeat(MAX_ARGUMENT_BYTES));
        assert!(huge.len() > MAX_ARGUMENT_BYTES);
        let error = coerce_args(&spec, &huge).unwrap_err();
        assert!(error.contains("the limit is"), "{}", &error[..error.len().min(120)]);
        assert!(error.len() < 500, "the refusal is {} bytes", error.len());

        // A maximal *legitimate* save_article is under the cap: 8 string arguments
        // at MAX_ARG_CHARS plus two arrays of MAX_ARG_ITEMS × MAX_ARG_ITEM. This is
        // the arithmetic behind the number, kept as a test so the two cannot drift.
        let save = crate::specs::tools()
            .into_iter()
            .find(|spec| spec.name == "save_article")
            .unwrap();
        let mut widest = Map::new();
        for key in [
            "url", "title", "text", "summary", "topic", "published", "publisher", "place",
        ] {
            widest.insert(key.to_string(), Value::String("x".repeat(MAX_ARG_CHARS)));
        }
        for key in ["tags", "categories"] {
            widest.insert(
                key.to_string(),
                Value::Array(
                    (0..MAX_ARG_ITEMS)
                        .map(|_| Value::String("y".repeat(MAX_ARG_ITEM)))
                        .collect(),
                ),
            );
        }
        let widest = Value::Object(widest).to_string();
        assert!(
            widest.len() < MAX_ARGUMENT_BYTES,
            "a schema-conforming call is {} bytes, over the {} cap",
            widest.len(),
            MAX_ARGUMENT_BYTES
        );
        let args = coerce_args(&save, &widest).expect("the widest real call still parses");
        assert_eq!(args["text"].as_str().unwrap().len(), MAX_ARG_CHARS);
        assert_eq!(args["tags"].as_array().unwrap().len(), MAX_ARG_ITEMS);
    }

    #[test]
    fn a_number_that_json_cannot_hold_is_refused_and_a_big_one_is_clamped() {
        // `NaN`/`Infinity` are not JSON: they are a refusal with a sentence, not a
        // value. A number that *is* JSON but out of the schema's range is clamped to
        // the schema's bound, and a float loses its fraction.
        let spec = search_spec();
        for text in [
            "{\"query\":\"x\",\"limit\":NaN}",
            "{\"query\":\"x\",\"limit\":Infinity}",
            "{\"query\":\"x\",\"limit\":-Infinity}",
            "{\"query\":\"x\",\"limit\":1e400}",
        ] {
            let error = coerce_args(&spec, text).unwrap_err();
            assert!(error.contains("not valid JSON"), "{}", text);
        }
        for (text, expected) in [
            ("{\"query\":\"x\",\"limit\":1e308}", json!(50)),
            ("{\"query\":\"x\",\"limit\":18446744073709551615}", json!(50)),
            ("{\"query\":\"x\",\"limit\":-5}", json!(1)),
            ("{\"query\":\"x\",\"limit\":0.0000001}", json!(1)),
        ] {
            let args = coerce_args(&spec, text).expect(text);
            assert_eq!(args["limit"], expected, "{}", text);
        }
    }

    #[test]
    fn arguments_that_are_not_an_object_are_refused_not_guessed() {
        // The attack list's shape: `arguments` is a string, a number, `null`, a
        // boolean or an array instead of the object the schema describes. Each is a
        // sentence the model can act on; none is coerced into an object, and none
        // reaches a handler.
        let spec = search_spec();
        for (text, expected) in [
            ("\"just a string\"", "must be a JSON object"),
            ("42", "must be a JSON object"),
            ("null", "must be a JSON object"),
            ("true", "must be a JSON object"),
            ("[1,2,3]", "not an array"),
            // A string that *contains* the object is still a string here: the JSON
            // has to be the arguments, not a quotation of them.
            ("\"{\\\"query\\\":\\\"x\\\"}\"", "must be a JSON object"),
        ] {
            let error = coerce_args(&spec, text).unwrap_err();
            assert!(error.contains(expected), "{} -> {}", text, error);
            assert!(error.contains("call search_news"), "{}", error);
            // The refusal names the arguments the tool does take.
            assert!(error.contains("query: string, required"), "{}", error);
        }
    }

    #[test]
    fn duplicate_keys_resolve_the_documented_way() {
        // JSON allows the same key twice and `serde_json` keeps the last one — the
        // same answer Python's `json` gave. Both the name and the arguments resolve
        // through the same parser, so a second key can never make the call that runs
        // differ from the call that was validated.
        let args = coerce_args(
            &search_spec(),
            "{\"query\":\"first\",\"query\":\"second\",\"limit\":5,\"limit\":50}",
        )
        .unwrap();
        assert_eq!(args["query"], json!("second"));
        assert_eq!(args["limit"], json!(50));
        let calls = calls_in_text("{\"name\":\"stats\",\"name\":\"search_news\",\"arguments\":{}}")
            .expect("one call");
        assert_eq!(calls[0].name(), "search_news");
    }
    /// `save_article`'s `entities` is the one argument that carries two answers per item, so
    /// the coercion has to keep the record: a name and the kind the model decided. Flattening
    /// it to a string is what would leave the graph with names and no kinds.
    #[test]
    fn a_record_array_keeps_its_fields_and_drops_what_is_not_one() {
        let tools = crate::specs::tools();
        let save = tools
            .iter()
            .find(|spec| spec.name == "save_article")
            .expect("save_article is a real tool");
        let raw = json!({
            "url": "https://example.test/a",
            "title": "T",
            "summary": "S",
            "topic": "Chips",
            "entities": [
                {"name": "  Olaf Scholz ", "type": "Person", "invented": "x"},
                {"name": "Nvidia", "type": "Organization"},
                {"name": "", "type": "Person"},
                "just a string",
                {"type": "City"},
            ],
        })
        .to_string();
        let args = coerce_args(save, &raw).expect("the arguments are the schema's");

        assert_eq!(
            args["entities"],
            json!([
                {"name": "Olaf Scholz", "type": "Person"},
                {"name": "Nvidia", "type": "Organization"}
            ]),
            "a record array stays records: the name cleaned, the kind kept, an undeclared \
             field and a nameless item dropped"
        );
        // A plain string array stays what it was: every other tool's list argument.
        let tags = json!({
            "url": "https://example.test/a", "title": "T", "summary": "S", "topic": "Chips",
            "tags": ["  chips  ", "export controls"],
        })
        .to_string();
        let args = coerce_args(save, &tags).expect("tags are the schema's");
        assert_eq!(args["tags"], json!(["chips", "export controls"]));
    }

}
