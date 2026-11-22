//! The chat-completions protocol as llama.cpp serves it, and the client that
//! speaks it.
//!
//! Two shapes are supported, because both exist in the wild:
//!
//! * **non-streaming** — one JSON body, `choices[0].message`;
//! * **streaming** — SSE frames of `choices[0].delta`, content and tool-call
//!   fragments arriving separately, with tool-call arguments split across frames
//!   by index. [`Aggregator`] puts them back together; a partial argument string
//!   is never parsed as JSON.
//!
//! Everything the model says is untrusted. This module parses and aggregates; it
//! does not interpret. Tool calls are handed on as text ([`RawCall`]) and only
//! become typed arguments in [`crate::tool`], against a schema.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::Control;
use crate::error::{error_detail, says_context_overflow, status_error, AiError};
use crate::health::ServerInfo;
use crate::sse::{SseEvent, SseParser};
use crate::endpoint::Endpoint;
use crate::http1::LoopbackHttp;

/// `ai.py` calls with `max_tokens=1200` for a small question and `2048` inside the
/// agent loop; one default, the loop's larger number.
/// The default answer budget, in tokens.
///
/// Eight thousand tokens of room, because a thinking turn spends most of its budget before
/// the answer starts: the measured verdict used 827 tokens to say fourteen of them, and a
/// real question on a real article costs more. A budget that fits only the thought is a
/// budget that returns an empty answer.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// `ai.py`'s agent temperature (0.25).
pub const DEFAULT_TEMPERATURE: f32 = 0.25;

/// `chat/completions` on a loopback server.
pub const COMPLETIONS: &str = "/v1/chat/completions";
/// The model list: the id the server answers with.
pub const MODELS: &str = "/v1/models";
/// llama.cpp's own settings endpoint: context size, slots, alias.
pub const PROPS: &str = "/props";

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

/// One message in the list sent to the model. The shape is llama.cpp's, field for
/// field: `content` is always present (an assistant turn that only calls tools
/// carries `""`), and the tool fields appear only where they apply.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: "system".to_string(),
            content: content.into(),
            ..ChatMessage::default()
        }
    }

    pub fn user(content: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: content.into(),
            ..ChatMessage::default()
        }
    }

    pub fn assistant(content: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: content.into(),
            ..ChatMessage::default()
        }
    }

    /// An assistant turn that asks for tools: `content` may be empty, and
    /// `tool_calls` is the array the model produced (kept verbatim).
    pub fn assistant_calls(content: impl Into<String>, tool_calls: Value) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: content.into(),
            tool_calls: Some(tool_calls),
            ..ChatMessage::default()
        }
    }

    /// One tool result, addressed to the call it answers.
    pub fn tool(call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
            ..ChatMessage::default()
        }
    }
}

// ---------------------------------------------------------------------------
// request / reply
// ---------------------------------------------------------------------------

/// One model call. `body()` is the exact JSON `ai.py` sent (same field names,
/// same `chat_template_kwargs`), so a model tuned on the Python layer sees an
/// identical request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// Wire-shaped tool array (see [`crate::tool::ToolRegistry::wire`]).
    pub tools: Option<Value>,
    pub tool_choice: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub stream: bool,
    pub enable_thinking: bool,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages,
            tools: None,
            tool_choice: None,
            temperature: DEFAULT_TEMPERATURE,
            max_tokens: DEFAULT_MAX_TOKENS,
            stream: false,
            // The model was chosen because it can reason, so it reasons. A caller that wants
            // a reflex answer (a one-word verdict in a tight loop) says so with
            // [`ChatRequest::thinking(false)`]; the default is not "do not think".
            enable_thinking: true,
        }
    }

    /// Turn the model's reasoning on or off for this call.
    ///
    /// With it off the server answers directly — measured on this machine: a one-line JSON
    /// verdict took **0.67 s** with `enable_thinking: false` and **45.7 s** with it on (827
    /// completion tokens, 3 533 characters of reasoning). That is the trade a caller makes:
    /// the batch passes set a budget and think, the tightest interactive path may not.
    pub fn thinking(mut self, thinking: bool) -> ChatRequest {
        self.enable_thinking = thinking;
        self
    }

    pub fn with_tools(mut self, tools: Value, choice: &str) -> ChatRequest {
        self.tools = Some(tools);
        self.tool_choice = Some(choice.to_string());
        self
    }

    pub fn streaming(mut self, stream: bool) -> ChatRequest {
        self.stream = stream;
        self
    }

    /// The exact JSON body of this call.
    ///
    /// The two sampling numbers are clamped by [`crate::sampling::bounded`] here,
    /// at the one place a request becomes bytes, so every caller — the agent loop,
    /// the CLI, `chat_simple`, a test — is bounded whether it thought about it or
    /// not. A `NaN` temperature would otherwise serialise as `"temperature": null`.
    pub fn body(&self) -> Value {
        let (temperature, max_tokens) = crate::sampling::bounded(self.temperature, self.max_tokens);
        let mut body = json!({
            "model": self.model,
            "messages": self.messages,
            "temperature": crate::sampling::wire_temperature(temperature),
            "max_tokens": max_tokens,
            // Qwen3.5 and friends: answer, do not think first.
            "chat_template_kwargs": { "enable_thinking": self.enable_thinking },
        });
        if let Some(object) = body.as_object_mut() {
            if let Some(tools) = &self.tools {
                object.insert("tools".to_string(), tools.clone());
                if let Some(choice) = &self.tool_choice {
                    object.insert("tool_choice".to_string(), Value::String(choice.clone()));
                }
            }
            if self.stream {
                object.insert("stream".to_string(), Value::Bool(true));
            }
        }
        body
    }

    pub fn body_text(&self) -> String {
        self.body().to_string()
    }
}

/// One tool call as the model wrote it: a name and an **unparsed** argument
/// string. Nothing here has been validated — that is [`crate::tool`]'s job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCall {
    pub id: String,
    pub name: String,
    /// JSON text, exactly as the model produced it (possibly invalid).
    pub arguments: String,
}

impl RawCall {
    /// The wire form kept in an assistant message so the tool result can answer it.
    pub fn to_wire(&self) -> Value {
        json!({
            "id": self.id,
            "type": "function",
            "function": { "name": self.name, "arguments": self.arguments },
        })
    }
}

/// One answer: the text, the thinking behind it, the raw calls, why it stopped, and what
/// it cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatReply {
    pub model: String,
    /// The answer proper — what the prompt asked for.
    pub content: String,
    /// The model's own reasoning, when it thought first (`reasoning_content`).
    ///
    /// This is **not** the answer and is never substituted for one: a page of deliberation
    /// parsed as JSON is a parse failure dressed as data, and a chat that showed it as the
    /// reply would be putting the model's working in its mouth. It is kept because it is
    /// the interesting part of a local model that can think — the window draws it while the
    /// answer is still arriving — and because "it thought for 3 500 characters and then ran
    /// out of budget" is the only honest diagnosis of an empty answer.
    pub thinking: String,
    pub calls: Vec<RawCall>,
    pub finish_reason: String,
    /// `usage.completion_tokens` when the server reported it.
    pub usage_tokens: Option<u64>,
    /// The stream ended mid-frame; anything half-written was dropped.
    pub truncated: bool,
}

impl ChatReply {
    /// True when the model asked for tools instead of answering.
    pub fn wants_tools(&self) -> bool {
        !self.calls.is_empty()
    }
}

// ---------------------------------------------------------------------------
// the trait
// ---------------------------------------------------------------------------

/// What the agent loop needs from a model. Injected, so the loop can be tested
/// without a model — and so a future `atlas-ai` can talk to something else on the
/// same loopback socket without the loop changing.
pub trait LlamaClient {
    /// The model id this client asks for.
    fn model(&self) -> &str;

    /// One non-streaming call.
    fn complete(&self, request: &ChatRequest) -> Result<ChatReply, AiError>;

    /// One streaming call. `on_delta` receives each content fragment as it
    /// arrives and can stop the stream by returning [`Control::Cancel`].
    fn complete_stream(
        &self,
        request: &ChatRequest,
        on_delta: &mut dyn FnMut(&str) -> Control,
    ) -> Result<ChatReply, AiError>;

    /// Is the server answering, which model, what context size?
    fn probe(&self) -> Result<ServerInfo, AiError>;
}

// ---------------------------------------------------------------------------
// frame decoding — pure, so it is tested without a socket
// ---------------------------------------------------------------------------

/// One `delta.tool_calls[]` element, possibly a fragment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeltaCall {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// What one SSE `data:` frame contained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Content, the model's thinking, and/or tool-call fragments.
    Delta {
        content: String,
        /// `reasoning_content` for this frame: the model talking to itself. Kept apart from
        /// `content` so a caller can draw it as working rather than as an answer.
        thinking: String,
        calls: Vec<DeltaCall>,
        finish_reason: Option<String>,
    },
}

/// Decode one SSE payload. `Ok(None)` means the frame carried nothing this crate
/// uses (an empty `choices`, a role-only first frame).
///
/// An error *frame* — `{"error":{"message":…}}` — becomes [`AiError::ModelError`],
/// or [`AiError::ContextOverflow`] when the text is the context refusal, so the
/// loop can retry with a smaller budget exactly as `ai.py` did.
pub fn decode_frame(data: &str) -> Result<Option<Frame>, AiError> {
    let value: Value = match serde_json::from_str(data) {
        Ok(value) => value,
        Err(_) => {
            return Err(AiError::BadResponse(
                "an SSE frame is not JSON".to_string(),
            ))
        }
    };
    if let Some(error) = value.get("error") {
        let detail = error_detail(&json!({ "error": error }).to_string());
        // No status here — an error frame travels inside a 200 stream — so the
        // words are the only evidence, and they are enough for this one case.
        if says_context_overflow(&detail) {
            return Err(AiError::ContextOverflow(detail));
        }
        return Err(AiError::ModelError(detail));
    }
    let choice = match value.get("choices").and_then(|c| c.get(0)) {
        Some(choice) => choice,
        None => return Ok(None),
    };
    let node = choice.get("delta").or_else(|| choice.get("message"));
    let mut content = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<DeltaCall> = Vec::new();
    if let Some(node) = node {
        if let Some(text) = node.get("content").and_then(|c| c.as_str()) {
            content.push_str(text);
        }
        if let Some(text) = node.get("reasoning_content").and_then(|c| c.as_str()) {
            thinking.push_str(text);
        }
        if let Some(array) = node.get("tool_calls").and_then(|c| c.as_array()) {
            for (position, item) in array.iter().enumerate() {
                let function = item.get("function");
                calls.push(DeltaCall {
                    index: item
                        .get("index")
                        .and_then(|i| i.as_u64())
                        .map(|i| i as usize)
                        .unwrap_or(position),
                    id: item.get("id").and_then(|i| i.as_str()).map(str::to_string),
                    name: function
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string),
                    arguments: function
                        .and_then(|f| f.get("arguments"))
                        .map(argument_text),
                });
            }
        }
    }
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .filter(|f| !f.is_empty())
        .map(str::to_string);
    if content.is_empty() && thinking.is_empty() && calls.is_empty() && finish_reason.is_none() {
        return Ok(None);
    }
    Ok(Some(Frame::Delta {
        content,
        thinking,
        calls,
        finish_reason,
    }))
}

/// A tool-call argument field as text: a string as-is, an object re-serialised,
/// anything else emptied (a number is not an argument list).
fn argument_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Object(_) | Value::Array(_) => value.to_string(),
        other => other.to_string(),
    }
}

/// Puts streamed fragments back together: content is concatenated, tool calls are
/// merged by `index` (id once, name and argument text appended).
///
/// At most [`MAX_CALLS_PER_TURN`] slots are kept, here where the slots are made. The
/// cap in [`crate::tool::turn_of_counted`] runs on the *collected* list, so on its
/// own it bounded the calls a turn may make but not the memory the stream could ask
/// for: a stream that walked `tool_calls[].index` from 0 to 999 999 grew a map slot
/// per frame before the first cap was ever consulted. A slot past the cap is
/// dropped, and the model is not asked about it — the fragments of a call that is
/// not going to run are not worth a sentence.
#[derive(Debug, Default)]
pub struct Aggregator {
    content: String,
    /// The model's reasoning, as it arrived. Drawn while the answer is still coming; never
    /// returned as the answer.
    thinking: String,
    calls: BTreeMap<usize, RawCall>,
    finish_reason: String,
    truncated: bool,
}

impl Aggregator {
    pub fn new() -> Aggregator {
        Aggregator::default()
    }

    pub fn push(&mut self, frame: Frame) {
        match frame {
            Frame::Delta {
                content,
                thinking,
                calls,
                finish_reason,
            } => {
                self.content.push_str(&content);
                self.thinking.push_str(&thinking);
                for call in calls {
                    // `contains_key` first: a fragment for a slot already held is
                    // always merged, however many slots the cap has kept.
                    let known = self.calls.contains_key(&call.index);
                    if !known && self.calls.len() >= crate::tool::MAX_CALLS_PER_TURN {
                        continue;
                    }
                    let slot = self.calls.entry(call.index).or_insert_with(|| RawCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(id) = call.id {
                        if !id.is_empty() {
                            slot.id = id;
                        }
                    }
                    if let Some(name) = call.name {
                        slot.name.push_str(&name);
                    }
                    if let Some(arguments) = call.arguments {
                        slot.arguments.push_str(&arguments);
                    }
                }
                if let Some(reason) = finish_reason {
                    self.finish_reason = reason;
                }
            }
        }
    }

    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    pub fn finish(self, model: &str) -> ChatReply {
        let mut calls: Vec<RawCall> = Vec::new();
        for (index, mut call) in self.calls {
            // A nameless call is kept with its empty name so the tool layer reports
            // it instead of silently dropping the turn.
            if call.id.is_empty() {
                call.id = format!("call_{}", index);
            }
            calls.push(call);
        }
        ChatReply {
            model: model.to_string(),
            content: self.content,
            thinking: self.thinking,
            calls,
            finish_reason: self.finish_reason,
            usage_tokens: None,
            truncated: self.truncated,
        }
    }

    /// The answer so far, for a caller that streams.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// The model's reasoning so far — what a window draws while it waits for the answer.
    pub fn thinking(&self) -> &str {
        &self.thinking
    }
}

// ---------------------------------------------------------------------------
// reply parsing (non-streaming)
// ---------------------------------------------------------------------------

/// Read `choices[0].message` out of a full answer.
pub fn parse_reply(body: &str, model: &str) -> Result<ChatReply, AiError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| AiError::BadResponse("the answer is not JSON".to_string()))?;
    if let Some(error) = value.get("error") {
        let detail = error_detail(&json!({ "error": error }).to_string());
        // No status here — an error frame travels inside a 200 stream — so the
        // words are the only evidence, and they are enough for this one case.
        if says_context_overflow(&detail) {
            return Err(AiError::ContextOverflow(detail));
        }
        return Err(AiError::ModelError(detail));
    }
    let choice = value
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| AiError::BadResponse("the answer has no choices".to_string()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| AiError::BadResponse("the answer has no message".to_string()))?;

    // Two fields, two meanings, never one standing in for the other. The old code
    // substituted `reasoning_content` for an empty `content`, which turned the model's
    // deliberation into "the answer": a JSON parser then saw prose, a news verdict saw a
    // paragraph of hedged thinking, and a reader saw the working as the conclusion.
    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let thinking = message
        .get("reasoning_content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let mut calls: Vec<RawCall> = Vec::new();
    if let Some(array) = message.get("tool_calls").and_then(|c| c.as_array()) {
        for (position, item) in array.iter().enumerate() {
            let function = item.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = function
                .and_then(|f| f.get("arguments"))
                .map(argument_text)
                .unwrap_or_default();
            let id = item
                .get("id")
                .and_then(|i| i.as_str())
                .filter(|i| !i.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{}", position));
            calls.push(RawCall {
                id,
                name,
                arguments,
            });
        }
    }
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("")
        .to_string();
    let usage_tokens = value
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64());
    let reported = value
        .get("model")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .unwrap_or(model)
        .to_string();
    Ok(ChatReply {
        model: reported,
        content,
        thinking,
        calls,
        finish_reason,
        usage_tokens,
        truncated: false,
    })
}

// ---------------------------------------------------------------------------
// the loopback client
// ---------------------------------------------------------------------------

/// The real client: llama.cpp on `127.0.0.1`, base URL and model from the caller.
#[derive(Debug, Clone)]
pub struct HttpLlamaClient {
    endpoint: Endpoint,
    model: String,
    http: LoopbackHttp,
}

impl HttpLlamaClient {
    /// `base_url` is the caller's configuration (e.g. `http://127.0.0.1:8080`);
    /// `timeout` is the hard deadline for one call, streaming included.
    pub fn new(base_url: &str, model: &str, timeout: Duration) -> Result<HttpLlamaClient, AiError> {
        let endpoint = Endpoint::parse(base_url)?;
        let http = LoopbackHttp::new(endpoint.clone(), timeout);
        Ok(HttpLlamaClient {
            endpoint,
            model: model.to_string(),
            http,
        })
    }

    pub fn with_max_body(mut self, max_body: usize) -> HttpLlamaClient {
        self.http = self.http.clone().with_max_body(max_body);
        self
    }

    /// How many transient failures to retry, clamped to
    /// [`crate::http1::MAX_RETRIES`].
    pub fn with_retries(mut self, retries: u8) -> HttpLlamaClient {
        self.http = self.http.clone().with_retries(retries);
        self
    }

    /// The same client with a different deadline per call.
    ///
    /// This used to rebuild the transport from the endpoint, which silently threw
    /// away a `max_body` or a `retries` set earlier in the chain — the order of two
    /// builder calls changed behaviour. It now changes one field.
    pub fn timeout(mut self, timeout: Duration) -> HttpLlamaClient {
        self.http = self.http.clone().with_timeout(timeout);
        self
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The transport, for a caller that measures what it was configured with.
    pub fn http(&self) -> &LoopbackHttp {
        &self.http
    }

    /// One GET, parsed as JSON. `(status, value)`.
    fn get(&self, path: &str) -> Result<(u16, Value), AiError> {
        let (status, text) = self.http.get_json(path)?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &text));
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| {
            AiError::BadResponse(format!("{} did not answer with JSON", path))
        })?;
        Ok((status, value))
    }
}

impl LlamaClient for HttpLlamaClient {
    fn model(&self) -> &str {
        &self.model
    }

    fn complete(&self, request: &ChatRequest) -> Result<ChatReply, AiError> {
        let mut request = request.clone();
        request.stream = false;
        let mut response = self.http.post_json(COMPLETIONS, &request.body_text())?;
        let status = response.status();
        let body = response.read_text()?;
        if !response.is_success() {
            // One decision, so a 500 is an `Http { status }` error everywhere.
            return Err(status_error(status, &body));
        }
        parse_reply(&body, &self.model)
    }

    fn complete_stream(
        &self,
        request: &ChatRequest,
        on_delta: &mut dyn FnMut(&str) -> Control,
    ) -> Result<ChatReply, AiError> {
        let mut request = request.clone();
        request.stream = true;
        let mut response = self.http.post_json(COMPLETIONS, &request.body_text())?;
        let status = response.status();
        if !response.is_success() {
            let body = response.read_text()?;
            return Err(status_error(status, &body));
        }
        let mut parser = SseParser::new();
        let mut aggregator = Aggregator::new();
        let mut buffer = [0u8; 8192];
        let mut cancelled = false;
        loop {
            let read = response.read_some(&mut buffer)?;
            if read == 0 {
                break;
            }
            for event in parser.feed(&buffer[..read])? {
                match event {
                    SseEvent::Done => {
                        // `[DONE]` is the end; a `finish_reason` may have come first.
                    }
                    SseEvent::Data(data) => {
                        if let Some(frame) = decode_frame(&data)? {
                            let text = match &frame {
                                Frame::Delta { content, .. } => content.clone(),
                            };
                            aggregator.push(frame);
                            if !text.is_empty() && on_delta(&text) == Control::Cancel {
                                cancelled = true;
                            }
                        }
                    }
                }
                if cancelled || parser.is_done() {
                    break;
                }
            }
            if cancelled {
                return Err(AiError::Cancelled);
            }
            if parser.is_done() {
                break;
            }
        }
        let finish = parser.finish()?;
        for event in finish.events {
            if let SseEvent::Data(data) = event {
                if let Some(frame) = decode_frame(&data)? {
                    aggregator.push(frame);
                }
            }
        }
        if finish.truncated {
            aggregator.mark_truncated();
        }
        let reply = aggregator.finish(&self.model);
        // A stream that carried nothing at all is not an answer: say so instead of
        // letting the loop treat silence as "the model chose not to use a tool".
        if reply.content.is_empty()
            && reply.calls.is_empty()
            && reply.finish_reason.is_empty()
            && reply.truncated
        {
            return Err(AiError::BadResponse(
                "the stream ended before the answer started".to_string(),
            ));
        }
        Ok(reply)
    }

    fn probe(&self) -> Result<ServerInfo, AiError> {
        let (_, models) = self.get(MODELS)?;
        let id = models
            .get("data")
            .and_then(|d| d.get(0))
            .and_then(|first| first.get("id"))
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or(&self.model)
            .to_string();
        let mut info = ServerInfo {
            answering: true,
            model: id,
            context_size: None,
            slots: None,
        };
        // `/props` is llama.cpp-specific: when it is absent the health check still
        // says "answering", it just cannot say how large the window is.
        if let Ok((_, props)) = self.get(PROPS) {
            info.context_size = props
                .get("default_generation_settings")
                .and_then(|s| s.get("n_ctx"))
                .and_then(|n| n.as_u64());
            info.slots = props
                .get("total_slots")
                .and_then(|n| n.as_u64())
                .or_else(|| props.get("n_ctx").and_then(|n| n.as_u64()).map(|_| 0))
                .filter(|n| *n > 0);
        }
        Ok(info)
    }
}

/// `ai.py::chat_simple` — one small question, no tools, thinking off.
///
/// The prompt is the caller's; this function only chooses the parameters the
/// Python layer used and hands back the text. `temperature` and `max_tokens` are
/// clamped where every request is clamped
/// ([`crate::sampling::bounded`], applied in [`ChatRequest::body`]): a `NaN`
/// temperature becomes [`DEFAULT_TEMPERATURE`] instead of the `null` a JSON body
/// cannot carry, and `max_tokens = 0` becomes 1 instead of an empty answer. A caller
/// that wants the documented pair for its job asks [`crate::sampling::Job::sampling`].
pub fn chat_simple(
    client: &dyn LlamaClient,
    user: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<String, AiError> {
    let mut request = ChatRequest::new(client.model().to_string(), vec![ChatMessage::user(user)]);
    request.temperature = temperature;
    request.max_tokens = max_tokens;
    let reply = client.complete(&request)?;
    Ok(reply.content)
}

/// The `{role, content}` pairs `ai.py::_history` kept from a client: no system
/// message, no tool message, nothing blank, each capped at 4000 characters, the
/// last 20 kept.
pub fn history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    const MAX_CHARS: usize = 4000;
    const KEEP: usize = 20;
    let mut out: Vec<ChatMessage> = Vec::new();
    for message in messages {
        if message.role != "user" && message.role != "assistant" {
            continue;
        }
        if message.content.trim().is_empty() {
            continue;
        }
        out.push(ChatMessage {
            role: message.role.clone(),
            content: crate::prompts::cut_chars(message.content.trim(), MAX_CHARS),
            ..ChatMessage::default()
        });
    }
    if out.len() > KEEP {
        out.split_off(out.len() - KEEP)
    } else {
        out
    }
}
