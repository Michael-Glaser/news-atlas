//! The one socket this window opens: a blocking HTTP/1.1 POST to the local model,
//! streamed back a frame at a time.
//!
//! Why by hand: the window ships three dependencies and none of them speaks HTTP, and
//! what is needed is one request line with a JSON body to a literal loopback address.
//! A crate for that would be a crate around `TcpStream::write_all` and `serde_json`,
//! for a protocol this file can state in full.
//!
//! What is decided here:
//!
//! * **Where** the model answers — [`endpoint`] accepts `http://` and a `127.0.0.1`
//!   literal only, so no name is ever resolved and no request can leave the machine;
//! * **what** is asked — [`body`]: one system prompt, the conversation, `stream: true`,
//!   and the [`Budget`] the reader set in Settings: `max_tokens` is their thinking room
//!   plus their answer room, and `chat_template_kwargs.enable_thinking` is asked for only
//!   when the thinking room is not zero. Nothing here decides those numbers; the window
//!   reads them from the archive and hands them down, and a machine nobody has configured
//!   sends exactly what this window always sent ([`Budget::SHIPPED`]);
//! * **how long** it may take — [`TIMEOUT`] bounds the connect, the write and *each*
//!   read, not the whole answer: a thinking model sends a frame every few milliseconds
//!   for forty seconds, and a wedged one is a sentence instead of a window that never
//!   returns;
//! * **what a reader sees while it runs** — the reply arrives as [`Delta`]s on two
//!   channels, `reasoning_content` ([`Delta::Thinking`]) and `content`
//!   ([`Delta::Answer`]), handed to the caller's sink as the frames land. The server's
//!   SSE bytes never leave this file in any other form;
//! * **what a reader is told** when there is no answer — [`ask`]'s sentences. The one
//!   that matters names the address it tried and the command that starts the server.
//!
//! What it does not do: retry, follow a redirect, speak TLS, read the archive or start
//! the model server. Every failure is `Err(String)` with one sentence for a reader — no
//! panic, no `unwrap`, and nothing that assumes a terminal.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{json, Value};

/// One read may take this long: the connect, the write, and each read of the stream.
///
/// Thirty seconds is generous for a single frame and short enough that a server which
/// accepts the connection and then says nothing is a sentence rather than a spinner. It
/// is *per read*, so a model that thinks for a minute keeps talking the whole time.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The wait for a turn that may be asked to think and to use tools.
///
/// One thinking call measured on this machine spent about 4 minutes and ~4 600 completion
/// tokens, and a tool step is another such call, so 30 s is a bound for a read of a page
/// and not for the model working. Ten minutes is one call's ceiling: the turn deadline and
/// the step budget are what stop a runaway, not this.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(600);

/// The path `llama-server` serves chat completions on.
pub const PATH: &str = "/v1/chat/completions";

/// The command that starts the model server on this platform (it starts `llama-server` as
/// a child of `atlas start`). Named in the refusal, because a reader who is told "not
/// answering" and not told how to start it has been told nothing they can act on.
///
/// The name is a platform decision made here and nowhere else: `./run.sh` on Linux and
/// macOS, because that is what a shell needs to run a file in the current directory, and
/// `run.ps1` on Windows, where there is no `./` to type and no `sh` to type it in. It is the
/// same answer `atlas-setup`'s check gives the setup row (`check::start_script_name`),
/// restated because the window does not link that crate; a reader told to start
/// `./run.sh` on a machine without one would be told nothing.
pub const START_COMMAND: &str = if cfg!(windows) { "run.ps1" } else { "./run.sh" };

/// What the page says when the worker thread ended without sending a result.
pub const NO_ANSWER: &str =
    "The answer never arrived from the model worker, so this question has none — ask again.";

/// The thinking budget this window asks for when nobody has said otherwise, in tokens.
///
/// The server runs with a 32 768-token context, and measured on this machine a one-line
/// JSON verdict took 827 completion tokens of thinking — so 8 192 is about ten of those,
/// and it is the number a reader on a fresh machine gets. Once the reader sets
/// `model_reasoning` / `model_answer` in Settings, the agent mirrors them into the
/// archive's `meta` table and [`Budget::from_meta`] is what builds the request instead.
pub const THINKING_TOKENS: u32 = 8_192;

/// The answer room a request gets when the reader set the thinking room and not this one:
/// the shared default from `atlas-config`'s spec table, restated here because the window
/// links no workspace crate.
pub const DEFAULT_ANSWER: u32 = 8_192;

/// The tokens one request may spend, as the reader set them.
///
/// Two numbers decide it: `model_reasoning` is the room the model may spend thinking and
/// `model_answer` the room for the answer that follows. llama.cpp takes *one* budget, so
/// the two are added when thinking is on — a model given only the answer's room would have
/// to answer inside it while thinking, which is how a small budget came back empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Ask the model to think (`chat_template_kwargs.enable_thinking`).
    pub thinking: bool,
    /// `max_tokens` on the wire: the thinking room plus the answer room, or the answer
    /// room alone when there is no thinking.
    pub max_tokens: u32,
}

impl Budget {
    /// The budget of a request that is not the Chat page's: thinking on, the two default
    /// rooms added up.
    ///
    /// Named so the tests that measured a whole request keep measuring one number, and so
    /// `ask` (the tool-free path) says what it spends in one place instead of an arithmetic
    /// result a later default could quietly move.
    pub const SHIPPED: Budget = Budget {
        thinking: true,
        max_tokens: THINKING_TOKENS + DEFAULT_ANSWER,
    };

    /// The note the Chat page shows when the reader has turned thinking off.
    ///
    /// Here, beside the flag it explains, so the page draws one sentence it did not write:
    /// a second copy of this wording in a view is a second thing to keep true.
    pub const REFLEX_NOTE: &'static str =
        "Thinking is off in Settings; this answer is reflex.";

    /// The budget the archive's `meta` table describes.
    ///
    /// Both values are the agent's own mirror of the settings it applied, as decimal text.
    /// Three rules, and each one is a case a reader can reach:
    ///
    /// * **nothing there** — the reader has never changed these — is the two defaults added
    ///   up, which is what the agent's own [`atlas_ai::Budgets::DEFAULT`] gives every other
    ///   part of the app: one request budget of 8 192 to think plus 8 192 to answer. A page
    ///   that asked for less than the agent does would be the one place a reader's default
    ///   window is not the window this app runs;
    /// * **a value that does not parse** is treated as absent, and an absent one takes the
    ///   shared default — never zero, because zero is the one value that means "off" and a
    ///   typo must not turn thinking off behind the reader's back;
    /// * **zero** is the reader saying so: `model_reasoning = 0` is no thinking at all, and
    ///   the request then carries no `enable_thinking` key and the answer's room alone.
    pub fn from_meta(reasoning: Option<&str>, answer: Option<&str>) -> Budget {
        let reasoning = reasoning.and_then(tokens);
        let answer = answer.and_then(tokens);
        let reasoning = reasoning.unwrap_or(THINKING_TOKENS);
        let answer = answer.unwrap_or(DEFAULT_ANSWER);
        Budget {
            thinking: reasoning > 0,
            // Saturating because the settings file is not the only writer of a `meta` row:
            // a pair of two enormous numbers must be a bad request, not an overflow panic
            // on the drawing thread.
            max_tokens: if reasoning > 0 {
                reasoning.saturating_add(answer)
            } else {
                answer
            },
        }
    }
}

/// One mirror value as a token count, or `None` when it is not a whole number.
fn tokens(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok()
}

/// The largest reply this will hold: a streaming answer is a few kB per second for a
/// minute, and a cap is what keeps a confused server from becoming the window's memory
/// problem.
const MAX_REPLY: usize = 8 * 1024 * 1024;

/// The largest single SSE frame, and the most of either channel that is kept.
///
/// A frame is one JSON delta — a few hundred bytes; 64 KiB is two orders of magnitude
/// more than that, so hitting it means the server is not speaking SSE at all. The text
/// caps are per channel, because a thinking model can write for a long time and the page
/// only ever draws the tail.
const MAX_FRAME: usize = 64 * 1024;
const MAX_TEXT: usize = 200_000;

const TEMPERATURE: f32 = 0.3;

/// One message on the wire. `role` is the two words the API names — the page decides
/// whose words these are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
    /// The call this message answers, for `role: "tool"`: the id the assistant gave.
    pub tool_call_id: Option<String>,
    /// The calls an assistant message asked for, already in the wire shape.
    pub tool_calls: Option<Value>,
}

impl Message {
    fn bare(role: &'static str, content: String) -> Message {
        Message {
            role,
            content,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Message {
        Message::bare("user", content.into())
    }

    pub fn assistant(content: impl Into<String>) -> Message {
        Message::bare("assistant", content.into())
    }

    /// One tool's answer, told which call it belongs to.
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Message {
        Message {
            role: "tool",
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls: None,
        }
    }

    /// An assistant message that asked for tools, so the next request can show the model
    /// what it already asked — the other half of the `tool_call_id` pairing.
    pub fn assistant_calls(content: impl Into<String>, calls: Value) -> Message {
        Message {
            role: "assistant",
            content: content.into(),
            tool_call_id: None,
            tool_calls: Some(calls),
        }
    }

    fn wire(&self) -> Value {
        let mut value = json!({ "role": self.role, "content": self.content });
        if let Some(id) = self.tool_call_id.as_ref() {
            value["tool_call_id"] = json!(id);
        }
        if let Some(calls) = self.tool_calls.as_ref() {
            value["tool_calls"] = calls.clone();
        }
        value
    }
}

/// One tool call the model asked for: the id to answer, the tool, and its JSON arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    /// The wire shape an assistant message carries, so the model can be shown its own
    /// request on the next step.
    pub fn wire(calls: &[ToolCall]) -> Value {
        Value::Array(
            calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments }
                    })
                })
                .collect(),
        )
    }

    /// A call as the page names it in one line: the tool and its arguments, short.
    ///
    /// The reader is told what is being looked up while it happens, so this is the tool's
    /// name and the arguments clipped at a word: a reader does not need the JSON.
    pub fn label(&self) -> String {
        let arguments = self.arguments.trim();
        if arguments.is_empty() || arguments == "{}" {
            return self.name.clone();
        }
        format!("{} {}", self.name, clip(arguments, 80))
    }
}

/// Tool calls arrive in fragments — an index, then pieces of the id, the name and the
/// arguments — so they are collected by index here and handed on whole.
///
/// Fragments are kept apart by index rather than by name: the name itself can arrive in
/// pieces, and the index is the only field the server sends whole in every fragment.
#[derive(Default)]
struct Calls {
    by_index: std::collections::BTreeMap<usize, ToolCall>,
}

/// One fragment of one call, as one frame carried it.
struct Fragment {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

impl Calls {
    fn absorb(&mut self, fragment: Fragment) {
        let call = self.by_index.entry(fragment.index).or_default();
        // The id is sent once, in the first fragment of the call; a later frame that
        // repeats it does not make it longer.
        if call.id.is_empty() {
            call.id = fragment.id;
        }
        call.name.push_str(&fragment.name);
        call.arguments.push_str(&fragment.arguments);
    }

    fn take(self) -> Vec<ToolCall> {
        self.by_index
            .into_values()
            .filter(|call| !call.name.trim().is_empty())
            .collect()
    }
}

/// One piece of a streamed reply, in the order the server sent it.
///
/// The two channels are never mixed: `reasoning_content` is what the model thought and
/// `content` is what it decided to say, and a page that substituted the first for the
/// second would be showing a working-out as an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// A piece of the model's thinking (`reasoning_content`).
    Thinking(String),
    /// A piece of the answer (`content`).
    Answer(String),
}

/// A whole reply: both channels, kept apart, plus what the server said about finishing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub thinking: String,
    pub answer: String,
    /// The model hit its token budget — the last frame's `finish_reason` was `length`.
    pub cut: bool,
    /// What the model asked the tools to do, in the order it asked. A turn that carries
    /// calls has no answer yet: the loop runs them and asks again.
    pub tool_calls: Vec<ToolCall>,
}

impl Reply {
    /// The number of characters of thinking, as a reader counts them.
    pub fn thought_chars(&self) -> usize {
        self.thinking.chars().count()
    }
}

/// Ask the model one question and wait for the whole answer.
///
/// Blocking, by design: the caller runs this off the drawing thread. The two channels
/// come back together, which is what a test or a caller with nowhere to draw wants; the
/// page calls [`ask_streaming`] and paints between frames.
///
/// The budget is [`Budget::SHIPPED`], not the reader's: this entry point has no archive to
/// read it from, and it exists for callers that were measured against the shipped numbers.
/// Anything a reader configures goes through [`ask_streaming`] or [`ask_within`].
pub fn ask(model_url: &str, system: &str, turns: &[Message]) -> Result<Reply, String> {
    let (host, address) = endpoint(model_url)?;
    let mut quiet = |_: Delta| {};
    let reply = exchange(
        address,
        &host,
        &body(system, turns, &[], Budget::SHIPPED),
        Budget::SHIPPED,
        TIMEOUT,
        &host,
        &mut quiet,
    )?;
    Ok(reply)
}

/// [`ask`] with the wait and the budget named by the caller.
///
/// The address check is stated here too instead of delegated: no entry point in this
/// file reaches the socket without `endpoint` having agreed to the address first, and the
/// source shows that order where a reader looks for it.
pub fn ask_within(
    model_url: &str,
    system: &str,
    turns: &[Message],
    budget: Budget,
    timeout: Duration,
) -> Result<Reply, String> {
    let (host, address) = endpoint(model_url)?;
    let mut quiet = |_: Delta| {};
    let reply = exchange(
        address,
        &host,
        &body(system, turns, &[], budget),
        budget,
        timeout,
        &host,
        &mut quiet,
    )?;
    Ok(reply)
}

/// [`ask`], handing every delta to `on` as it arrives, with `tools` on the request: the
/// page's entry point, and the loop's.
///
/// The sink is called on the reading thread, in frame order, and is never called after
/// this returns. A delta is a piece of one channel — the caller decides where it goes. An
/// empty `tools` sends no menu at all, which is how the loop's last step is asked.
pub fn ask_streaming(
    model_url: &str,
    system: &str,
    turns: &[Message],
    tools: &[Value],
    budget: Budget,
    timeout: Duration,
    on: &mut dyn FnMut(Delta),
) -> Result<Reply, String> {
    let (host, address) = endpoint(model_url)?;
    let reply = exchange(
        address,
        &host,
        &body(system, turns, tools, budget),
        budget,
        timeout,
        &host,
        on,
    )?;
    Ok(reply)
}

/// The url's body after an `http://` scheme, or `None` for anything else.
///
/// The scheme is compared **case-insensitively**: RFC 3986 §3.1 makes it case-insensitive,
/// so `HTTP://127.0.0.1:8080` names the same origin as `http://127.0.0.1:8080`, and a
/// reader who pastes a url the shell or a chat client upper-cased should not be told their
/// address is wrong. Only `http` is accepted: `https`, `ftp`, a bare `127.0.0.1:8080` and
/// `//127.0.0.1:8080` all answer `None`, because the model is read over plain http on this
/// machine and a client that quietly spoke something else would be a different transport.
fn after_http_scheme(url: &str) -> Option<&str> {
    // `get(..7)` is a byte slice of exactly the scheme, so a url too short to hold one is
    // `None` rather than a panic; 7 ASCII bytes are a char boundary, so the slice below is
    // always valid.
    url.get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .map(|_| &url[7..])
}

/// The first `?` or `#` in the url body, with the character that was found.
///
/// The byte offset is reported rather than the text around it: the url comes from
/// `ATLAS_MODEL_URL` and may carry a control character, so a sentence quotes positions,
/// never the value.
fn first_query_or_fragment(rest: &str) -> Option<(usize, char)> {
    rest.char_indices()
        .find(|(_, ch)| *ch == '?' || *ch == '#')
}

/// The address a `model_url` names: the `Host:` header text and the socket.
///
/// A literal `127.0.0.1` and an explicit port, or a sentence. Not a host allowlist: an
/// address rule. This window talks to the model on this machine and to nothing else, so a
/// name — even `localhost`, even one that resolves to loopback — is refused rather than
/// resolved, and so is every other spelling of loopback: the model server is on the
/// literal, and a request that reaches a different address is a different request.
///
/// The sentences name the address that would work, never the value they refused: the url
/// comes from `ATLAS_MODEL_URL`, so it can carry a user name, a password or a control
/// character, and a refusal that echoed it would put that on screen (and, with a newline
/// in it, on a second line).
pub fn endpoint(model_url: &str) -> Result<(String, SocketAddr), String> {
    let url = model_url.trim();
    let rest = match after_http_scheme(url) {
        Some(rest) => rest,
        None => {
            return Err(
                "The model address is not an http:// url — the model is read over http on \
                 127.0.0.1 only."
                    .to_string(),
            )
        }
    };
    // A query or a fragment is refused before the address is even read: the request line
    // is a constant, so there is no place to put a query, and a fragment never leaves the
    // client. Both used to fall through to the port or the user-name check and come back
    // with a sentence about the wrong thing (`?x=1` was "not a number", `#@evil` was a
    // "user name"); naming the character is the whole difference between a sentence a
    // reader can act on and one that sends them looking at the wrong end of the url.
    if let Some((at, which)) = first_query_or_fragment(rest) {
        return Err(match which {
            '?' => format!(
                "The model address carries a query (the ? at byte {at}) — the request line is \
                 always /v1/chat/completions, so there is nowhere to put one; write \
                 http://127.0.0.1:8081 and ask in the chat."
            ),
            _ => format!(
                "The model address carries a fragment (the # at byte {at}) — a fragment is \
                 never sent to a server, so this window refuses it; write \
                 http://127.0.0.1:8081."
            ),
        });
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    // A path is not part of the dial — the request line is always [`PATH`] — so a path
    // that climbs is a value somebody got wrong, and it is refused where it is read.
    if path.split('/').any(|segment| segment == "..") {
        return Err(
            "The model address has a \"..\" in its path — the request line is always \
             /v1/chat/completions, so a path that climbs is not a route this window takes."
                .to_string(),
        );
    }
    if authority.contains('@') {
        return Err(
            "The model address carries a user name — the model server on this machine takes \
             none."
                .to_string(),
        );
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port),
        None => {
            return Err(
                "The model address names no port — give the model server's port, for example \
                 http://127.0.0.1:8081."
                    .to_string(),
            )
        }
    };
    if host != "127.0.0.1" {
        return Err(
            "The model address is not 127.0.0.1 — this window reads the model on this machine \
             only, so write the literal, for example http://127.0.0.1:8081."
                .to_string(),
        );
    }
    let port = port.parse::<u16>().map_err(|_| {
        "The model address has a port that is not a number — the model server listens on a \
         port such as 8081."
            .to_string()
    })?;
    if port == 0 {
        // Port 0 is "any port": a connect would be refused by the kernel, and the reader
        // would get a connection error instead of the one sentence that says the address
        // is not an address.
        return Err(
            "The model address asks for port 0 — a port 0 is any port, so write the port the \
             model server listens on, for example 8081."
                .to_string(),
        );
    }
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    Ok((format!("127.0.0.1:{port}"), address))
}

/// The request body: the system prompt, then the conversation, exactly as `atlas-ai`
/// sends it to this `llama-server`.
///
/// Built with `serde_json`, never with `format!`: a question may hold a quote, a
/// newline or a backslash, and the one place those are escaped is the one place that
/// knows JSON. `stream: true` is stated rather than implied, and `enable_thinking` is
/// asked for only when the [`Budget`] has thinking in it — a model told to think while
/// given the answer's room alone spends that room thinking and answers nothing.
fn body(system: &str, turns: &[Message], tools: &[Value], budget: Budget) -> String {
    let mut messages: Vec<Value> = Vec::with_capacity(turns.len() + 1);
    messages.push(json!({ "role": "system", "content": system }));
    for turn in turns {
        messages.push(turn.wire());
    }
    let mut body = json!({
        // `llama-server` serves one model per process; the name is not a selector.
        "model": "local",
        "messages": messages,
        "temperature": TEMPERATURE,
        // One number for thinking and answering together, because llama.cpp has one: the
        // reader's two rooms were added by `Budget` before they reached this line.
        "max_tokens": budget.max_tokens,
        "stream": true,
    });
    if budget.thinking {
        // Qwen3.5 and friends: think first, then answer. Absent — not `false` — when the
        // reader turned thinking off, so the template is left to answer directly.
        body["chat_template_kwargs"] = json!({ "enable_thinking": true });
    }
    if !tools.is_empty() {
        // Only when the caller has tools: a turn that must answer without looking
        // anything up (the loop's last step) is asked without a menu, so the model cannot
        // stall by asking for the same tool again.
        body["tools"] = Value::Array(tools.to_vec());
        body["tool_choice"] = json!("auto");
    }
    body.to_string()
}

/// The bytes of one request. `Content-Length` is the body's byte length, not its
/// character count: a question with an umlaut in it is a longer body than it looks.
fn request(host: &str, payload: &str) -> String {
    format!(
        "POST {PATH} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Accept: text/event-stream, application/json\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\
         \r\n\
         {payload}",
        length = payload.len()
    )
}

/// One whole conversation with the socket, on the reading thread.
///
/// Connect, write, read the head, and then take the body the way it is framed: an
/// event stream is handed on frame by frame through `on`, a plain JSON body is read
/// whole. `at` is only ever used to word a failure, and both paths end in the same
/// question — is there an answer in this? ([`finish`]).
fn exchange(
    address: SocketAddr,
    host: &str,
    payload: &str,
    budget: Budget,
    timeout: Duration,
    at: &str,
    on: &mut dyn FnMut(Delta),
) -> Result<Reply, String> {
    let request = request(host, payload);
    let mut stream =
        TcpStream::connect_timeout(&address, timeout).map_err(|_| not_answering(at))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| closed(at))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| closed(at))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|_| closed(at))?;
    stream.flush().map_err(|_| closed(at))?;

    let mut response: Vec<u8> = Vec::with_capacity(8 * 1024);
    read_head(&mut stream, &mut response, timeout, at)?;
    let head = head(&response)?;
    if head.stream && head.status == 200 {
        let reply = read_stream(&mut stream, &mut response, head, timeout, at, on)?;
        return finish(reply, at, budget.max_tokens);
    }
    read_whole(&mut stream, &mut response, head, timeout, at)?;
    finish(whole_reply(&response, head, at)?, at, budget.max_tokens)
}

/// Read until the head is complete. Everything that arrived is in `response`.
fn read_head(
    stream: &mut TcpStream,
    response: &mut Vec<u8>,
    timeout: Duration,
    at: &str,
) -> Result<(), String> {
    let mut chunk = [0u8; 8 * 1024];
    loop {
        if find(response, b"\r\n\r\n").is_some() {
            return Ok(());
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                // Nothing at all before the head: the server closed without answering.
                // A head that never completed is the same sentence.
                return Err(closed(at));
            }
            Ok(read) => {
                response.extend_from_slice(&chunk[..read]);
                if response.len() > MAX_FRAME {
                    return Err(too_long(at));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(silent(at, timeout))
            }
            Err(_) => return Err(closed(at)),
        }
    }
}

/// Read the rest of a reply that is not an event stream, until it is whole or the
/// connection ends. Everything that arrived is in `response`.
fn read_whole(
    stream: &mut TcpStream,
    response: &mut Vec<u8>,
    head: Head,
    timeout: Duration,
    at: &str,
) -> Result<(), String> {
    let mut chunk = [0u8; 8 * 1024];
    if whole(response) {
        return Ok(());
    }
    loop {
        match stream.read(&mut chunk) {
            // The connection ended before the length it promised: half a reply is not one,
            // and the sentence says so instead of handing the bytes on.
            Ok(0) if head.length.is_some_and(|length| response.len() < head.body_at + length) => {
                return Err(broken_chunk())
            }
            Ok(0) => return Ok(()),
            Ok(read) => {
                response.extend_from_slice(&chunk[..read]);
                if response.len() > MAX_REPLY {
                    return Err(too_long(at));
                }
                if whole(response) {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(silent(at, timeout))
            }
            Err(_) => return Err(closed(at)),
        }
    }
}

/// A parsed response head: the status, where the body starts, how the body is
/// framed, and whether it is an event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Head {
    status: u16,
    body_at: usize,
    length: Option<usize>,
    chunked: bool,
    /// `Content-Type: text/event-stream`: the body is frames, not one JSON document.
    stream: bool,
}

/// The head of `response`, or a sentence when it does not hold one yet.
///
/// One parser for both questions this file asks of a reply — "is it whole?" and "what
/// is in it?" — so a header is read by the same rules on both paths.
fn head(response: &[u8]) -> Result<Head, String> {
    let end = find(response, b"\r\n\r\n").ok_or_else(|| {
        "The local model's reply had no complete HTTP header in it — check data/llama.log."
            .to_string()
    })?;
    let text = String::from_utf8_lossy(&response[..end]);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            "The local model's reply did not begin with an HTTP status — check data/llama.log."
                .to_string()
        })?;
    let mut length = None;
    let mut chunked = false;
    let mut stream = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => length = value.parse::<usize>().ok(),
            "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
            // The one header that changes the shape of the body: frames, not a document.
            // Read here rather than sniffed from the bytes, so a JSON reply whose text
            // happens to hold `data:` is still read as JSON.
            "content-type" => stream = value.to_ascii_lowercase().contains("text/event-stream"),
            _ => {}
        }
    }
    Ok(Head {
        status,
        body_at: end + 4,
        length,
        chunked,
        stream,
    })
}

/// Is `response` a whole reply? Only used for a body that is one JSON document.
fn whole(response: &[u8]) -> bool {
    let Ok(head) = head(response) else {
        return false;
    };
    let body = &response[head.body_at..];
    if head.chunked {
        // The last chunk is a zero length line followed by the end of the trailers.
        return body.ends_with(b"0\r\n\r\n");
    }
    match head.length {
        Some(length) => body.len() >= length,
        // No length and no chunking: the body ends where the connection does.
        None => false,
    }
}

/// The status and the bytes of a whole reply.
fn split(response: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let head = head(response)?;
    let body = &response[head.body_at..];
    if head.chunked {
        return Ok((head.status, dechunk(body)?));
    }
    match head.length {
        Some(length) if body.len() >= length => Ok((head.status, body[..length].to_vec())),
        _ => Ok((head.status, body.to_vec())),
    }
}

/// Undo `Transfer-Encoding: chunked`, which a proxy in front of the server may add.
///
/// Small on purpose: a size line, its bytes, a CRLF, then a zero size. Hex parses with
/// `from_str_radix`, quotes in the size line (`1a;ext=1`) are cut off, and anything
/// else is a sentence — never a partial answer.
fn dechunk(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    let mut at = 0usize;
    loop {
        let line_end = match find(&body[at..], b"\r\n") {
            Some(offset) => at + offset,
            None => return Err(broken_chunk()),
        };
        let size_text = std::str::from_utf8(&body[at..line_end]).map_err(|_| broken_chunk())?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| broken_chunk())?;
        at = line_end + 2;
        if size == 0 {
            return Ok(out);
        }
        let stop = at + size;
        if stop > body.len() {
            return Err(broken_chunk());
        }
        out.extend_from_slice(&body[at..stop]);
        at = stop + 2;
    }
}

// ---------------------------------------------------------------------------
// the body as it arrives
// ---------------------------------------------------------------------------

/// The body of a streamed reply, taken the way the server frames it.
///
/// Two layerings in one place: chunked transfer coding (a proxy, or `llama-server`'s own
/// writer) is undone first, and the decoded bytes are split into server-sent-event frames
/// on the blank line that ends one. Both are incremental: a read can end in the middle of
/// a chunk header, a multi-byte character or a frame, and the piece is kept until the rest
/// arrives — which is why this is a struct and not a function over a buffer.
struct Framer {
    chunked: bool,
    /// Bytes read from the socket that chunk framing has not been able to peel yet.
    raw: Vec<u8>,
    /// Decoded bytes that are not yet a whole frame.
    text: Vec<u8>,
    /// The chunked body said it was finished (a zero-length chunk arrived).
    ended: bool,
}

impl Framer {
    fn new(chunked: bool) -> Framer {
        Framer {
            chunked,
            raw: Vec::new(),
            text: Vec::new(),
            ended: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.raw.extend_from_slice(bytes);
    }

    /// Every whole frame that the bytes so far allow, oldest first.
    fn frames(&mut self) -> Result<Vec<String>, String> {
        if self.chunked {
            self.peel_chunks()?;
        } else if !self.raw.is_empty() {
            let raw = std::mem::take(&mut self.raw);
            self.text.extend_from_slice(&raw);
        }
        let mut out: Vec<String> = Vec::new();
        loop {
            let Some((end, skip)) = frame_end(&self.text) else {
                break;
            };
            let frame: Vec<u8> = self.text.drain(..end + skip).take(end).collect();
            out.push(String::from_utf8_lossy(&frame).into_owned());
        }
        // A frame that has grown past the cap is not one delta of anything: the server
        // is not speaking SSE, and the window is not going to hold the reason why.
        if self.text.len() > MAX_FRAME {
            return Err(not_frames());
        }
        Ok(out)
    }

    /// Is there a piece of a frame waiting for bytes that never came?
    fn partial(&self) -> bool {
        !self.text.is_empty() && !self.ended
    }

    /// Move complete chunks out of `raw` and into `text`.
    fn peel_chunks(&mut self) -> Result<(), String> {
        loop {
            let Some(line_end) = find(&self.raw, b"\r\n") else {
                return Ok(());
            };
            let line = &self.raw[..line_end];
            let size_text = std::str::from_utf8(line).map_err(|_| broken_chunk())?;
            let size_text = size_text.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|_| broken_chunk())?;
            if size == 0 {
                // The trailers after the last chunk are not part of the body.
                self.ended = true;
                self.raw.clear();
                return Ok(());
            }
            if self.raw.len() < line_end + 2 + size + 2 {
                return Ok(());
            }
            let body: Vec<u8> = self.raw[line_end + 2..line_end + 2 + size].to_vec();
            self.text.extend_from_slice(&body);
            self.raw.drain(..line_end + 2 + size + 2);
        }
    }
}

/// `(end of frame, length of the blank line after it)` for the first whole frame.
fn frame_end(text: &[u8]) -> Option<(usize, usize)> {
    if let Some(end) = find(text, b"\n\n") {
        return Some((end, 2));
    }
    find(text, b"\r\n\r\n").map(|end| (end, 4))
}

/// What one SSE frame says.
enum Piece {
    /// A comment or a keep-alive: the server said nothing this time.
    Nothing,
    /// `data: [DONE]`: the stream is complete.
    Done,
    /// One delta, any of whose fields may be empty.
    Delta {
        thinking: String,
        content: String,
        finish: Option<String>,
        /// Fragments of the tool calls this delta carried, in the order they came.
        calls: Vec<Fragment>,
    },
}

/// Read the stream to its end, handing every delta to `on` as it arrives.
///
/// The loop is the whole point: each read is bounded by `timeout` and the answer is not,
/// so a model that thinks for forty seconds is forty seconds of frames rather than one
/// silent wait. A frame that never completes is a cut stream, not a partial answer.
fn read_stream(
    stream: &mut TcpStream,
    response: &mut Vec<u8>,
    head: Head,
    timeout: Duration,
    at: &str,
    on: &mut dyn FnMut(Delta),
) -> Result<Reply, String> {
    let mut framer = Framer::new(head.chunked);
    framer.push(&response[head.body_at..]);
    response.clear();
    let mut reply = Reply::default();
    let mut calls = Calls::default();
    let mut chunk = [0u8; 8 * 1024];
    let mut read_total = 0usize;
    let mut complete = false;
    'stream: loop {
        for frame in framer.frames()? {
            match piece(&frame)? {
                Piece::Nothing => {}
                Piece::Done => {
                    complete = true;
                    break 'stream;
                }
                Piece::Delta {
                    thinking,
                    content,
                    finish,
                    calls: fragments,
                } => {
                    for fragment in fragments {
                        calls.absorb(fragment);
                    }
                    if !thinking.is_empty() {
                        absorb(&mut reply.thinking, &thinking);
                        on(Delta::Thinking(thinking));
                    }
                    if !content.is_empty() {
                        absorb(&mut reply.answer, &content);
                        on(Delta::Answer(content));
                    }
                    if finish.as_deref() == Some("length") {
                        reply.cut = true;
                    }
                }
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                read_total += read;
                if read_total > MAX_REPLY {
                    return Err(too_long(at));
                }
                framer.push(&chunk[..read]);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(silent(at, timeout))
            }
            Err(_) => return Err(closed(at)),
        }
    }
    reply.tool_calls = calls.take();
    if !complete && framer.partial() {
        return Err(broken_chunk());
    }
    Ok(reply)
}

/// One frame's `data:` payload, joined and parsed.
fn piece(frame: &str) -> Result<Piece, String> {
    let mut parts: Vec<&str> = Vec::new();
    for line in frame.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            parts.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    let data = parts.join("\n");
    let data = data.trim();
    if data.is_empty() {
        return Ok(Piece::Nothing);
    }
    if data == "[DONE]" {
        return Ok(Piece::Done);
    }
    let value: Value = serde_json::from_str(data).map_err(|_| not_frames())?;
    if let Some(error) = value.get("error") {
        return Err(refused_error(error));
    }
    let delta = value.pointer("/choices/0/delta");
    let text = |key: &str| -> String {
        delta
            .and_then(|delta| delta.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut calls: Vec<Fragment> = Vec::new();
    if let Some(list) = delta
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for (position, call) in list.iter().enumerate() {
            let string = |key: &str| -> String {
                call.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let function = |key: &str| -> String {
                call.pointer("/function")
                    .and_then(|function| function.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            calls.push(Fragment {
                index: call
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|index| index as usize)
                    .unwrap_or(position),
                id: string("id"),
                name: function("name"),
                arguments: function("arguments"),
            });
        }
    }
    Ok(Piece::Delta {
        thinking: text("reasoning_content"),
        content: text("content"),
        finish: value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        calls,
    })
}

/// Keep `piece` on the end of `held`, up to the cap. Bytes are compared, not characters:
/// byte length is never smaller than character count, so the cap holds either way.
fn absorb(held: &mut String, piece: &str) {
    if held.len() + piece.len() > MAX_TEXT {
        return;
    }
    held.push_str(piece);
}

/// One JSON document instead of a stream: a server that answers a streaming request in
/// one piece (or a proxy that buffered it) is read by the same rules.
///
/// The three shapes that are not an answer: an `error` object (a refused request), no
/// `choices[0].message.content`, and an empty one. Each becomes a sentence, because the
/// page has nowhere to put a stack trace and a reader cannot read one.
fn reply_from(body: &str) -> Result<Reply, String> {
    let value: Value = serde_json::from_str(body).map_err(|_| not_json())?;
    if let Some(error) = value.get("error") {
        return Err(refused_error(error));
    }
    let message = value.pointer("/choices/0/message");
    let field = |key: &str| -> String {
        message
            .and_then(|message| message.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Ok(Reply {
        thinking: field("reasoning_content"),
        answer: field("content"),
        cut: value.pointer("/choices/0/finish_reason").and_then(Value::as_str) == Some("length"),
        tool_calls: whole_calls(message),
    })
}

/// The calls a non-streamed `choices[0].message` asked for, in the order it asked.
///
/// The shape differs from the streamed one: here each call is whole, so there is nothing
/// to collect by index — only to read out of the message.
fn whole_calls(message: Option<&Value>) -> Vec<ToolCall> {
    let Some(list) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|call| {
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.trim().is_empty() {
                return None;
            }
            Some(ToolCall {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name,
                // `arguments` is a JSON *string* by the API's own definition; a server
                // that sent an object is answered with the object, so a handler that
                // parses the text gets something rather than nothing.
                arguments: match call.pointer("/function/arguments") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                },
            })
        })
        .collect()
}

/// The reply of a whole (non-streamed) response, status and all.
fn whole_reply(response: &[u8], head: Head, at: &str) -> Result<Reply, String> {
    if head.status != 200 {
        return Err(status_refused(at, head.status));
    }
    let (_, body) = split(response)?;
    reply_from(&String::from_utf8_lossy(&body))
}

/// Is there an answer in this reply?
///
/// One place decides, so "the model thought and then had nothing to say" cannot be drawn
/// as an empty answer. A turn that asked for tools has no answer *yet* and is not a
/// failure: the loop runs what it asked for and asks again. A reply that hit the budget
/// while still thinking is the case the reader has to hear about by name — and `sent` is
/// the `max_tokens` this request carried, so the number the sentence quotes is the one the
/// reader's own settings produced, not the shipped constant.
fn finish(reply: Reply, _at: &str, sent: u32) -> Result<Reply, String> {
    if !reply.tool_calls.is_empty() {
        return Ok(reply);
    }
    if !reply.answer.trim().is_empty() {
        return Ok(reply);
    }
    if reply.cut && reply.thought_chars() > 0 {
        return Err(ran_out_of_room(sent));
    }
    if reply.thought_chars() > 0 {
        return Err(thought_but_no_answer(reply.thought_chars()));
    }
    Err(answered_nothing())
}

/// The first `needle` in `haystack`. `windows` rather than a regex, which is a crate.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A detail from the server, cut at a word boundary: it is the server's text, so it is
/// as long as the server felt like.
fn clip(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(limit).collect();
    if let Some(cut) = out.rfind(' ') {
        out.truncate(cut);
    }
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// the sentences: one per way there is no answer
// ---------------------------------------------------------------------------

fn not_answering(at: &str) -> String {
    format!(
        "The local model is not answering on {at} — start it with {START_COMMAND}, which launches \
         llama-server as a child of atlas start."
    )
}

/// The wait is named from the bound that actually ran, so a caller that asked for a
/// short one is told the truth about it.
fn silent(at: &str, waited: Duration) -> String {
    format!(
        "The local model on {at} accepted the connection and then sent nothing for {} — check \
         data/llama.log, which is where llama-server writes why.",
        bound(waited)
    )
}

/// A duration as a reader reads it: seconds above one, milliseconds below.
///
/// A 120 ms bound printed with `as_secs` is "0 s", which is not a wait anyone had; the
/// unit follows the bound so the sentence stays true when a test asks for a short one.
fn bound(waited: Duration) -> String {
    if waited >= Duration::from_secs(1) {
        format!("{} s", waited.as_secs())
    } else {
        format!("{} ms", waited.as_millis())
    }
}

fn closed(at: &str) -> String {
    format!(
        "The local model on {at} closed the connection before answering — check data/llama.log, \
         then ask again."
    )
}

fn too_long(at: &str) -> String {
    format!(
        "The local model on {at} answered more than {} kB — ask a narrower question.",
        MAX_REPLY / 1024
    )
}

fn status_refused(at: &str, status: u16) -> String {
    format!(
        "The local model on {at} answered HTTP {status} — check data/llama.log, then ask again \
         with a shorter question."
    )
}

fn broken_chunk() -> String {
    "The local model's reply was cut short mid-frame — check data/llama.log, then ask again."
        .to_string()
}

/// The body was not frames at all: one oversized frame, or JSON where an event stream
/// was announced. Nothing of it is drawn, because none of it is an answer.
fn not_frames() -> String {
    "The local model's stream was not made of frames — check data/llama.log, then ask again."
        .to_string()
}

fn not_json() -> String {
    "The local model answered with something that is not JSON — check data/llama.log.".to_string()
}

fn refused_error(error: &Value) -> String {
    let detail = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("no reason was given");
    format!(
        "The local model refused the question: {}.",
        clip(detail, 160)
    )
}

fn answered_nothing() -> String {
    "The local model answered nothing — ask again, or rephrase the question.".to_string()
}

/// The budget ran out while the model was still thinking: no answer arrived, and the
/// number the reader has to change is in the sentence.
///
/// `sent` is what this request carried — the reader's `model_reasoning + model_answer`, or
/// the shipped 8 192 on a machine nobody has configured. It used to be
/// [`THINKING_TOKENS`], which was a lie the moment Settings could change the number: a
/// reader who raised the budget to 16 000 and still ran out was told about 8 192.
fn ran_out_of_room(sent: u32) -> String {
    format!(
        "The local model ran out of room while thinking — its {sent}-token budget went on \
         thoughts, so ask something narrower, or raise the thinking room in Settings."
    )
}

/// Thinking arrived, the stream ended, and the answer never started.
fn thought_but_no_answer(chars: usize) -> String {
    format!(
        "The local model thought for {chars} characters and then stopped without an answer — ask \
         again, or narrow the question."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `127.0.0.1` and a port, or a sentence: the address rule, not a host list.
    #[test]
    fn the_address_is_a_loopback_literal_with_a_port() {
        let (host, address) = endpoint("http://127.0.0.1:8081").expect("a loopback address");
        assert_eq!(host, "127.0.0.1:8081");
        assert_eq!(address.to_string(), "127.0.0.1:8081");

        // A trailing slash and surrounding space are the same address.
        let (host, address) = endpoint("  http://127.0.0.1:8081/  ").expect("the same address");
        assert_eq!(host, "127.0.0.1:8081");
        assert!(address.ip().is_loopback());
        // A path after the port is not part of the address this file posts to.
        let (host, _) = endpoint("http://127.0.0.1:9000/anything").expect("a port and a path");
        assert_eq!(host, "127.0.0.1:9000");
    }

    /// RFC 3986 §3.1: the scheme is case-insensitive, so every spelling of `http` is the
    /// same origin and has to reach the same address. Only the spelling varies here — the
    /// literal, the port and the rule are untouched.
    #[test]
    fn a_scheme_case_is_the_same_origin() {
        for spelling in [
            "http://127.0.0.1:8081",
            "HTTP://127.0.0.1:8081",
            "Http://127.0.0.1:8081",
            "hTtP://127.0.0.1:8081",
            "  HTTP://127.0.0.1:8081  ",
        ] {
            let (host, address) = endpoint(spelling).unwrap_or_else(|why| panic!("{spelling}: {why}"));
            assert_eq!(host, "127.0.0.1:8081", "{spelling}");
            assert_eq!(address.to_string(), "127.0.0.1:8081", "{spelling}");
        }
        // A scheme is `http` or a sentence: case never makes something else acceptable.
        for spelling in [
            "HTTPS://127.0.0.1:8081",
            "FTP://127.0.0.1:8081",
            "httpss://127.0.0.1:8081",
            // Six characters of "http://" is not the scheme.
            "http:/127.0.0.1:8081",
            "HTTP:/127.0.0.1:8081",
            // The case rule does not relax the address rule.
            "HTTP://localhost:8081",
            "HTTP://10.1.2.3:8081",
            "HTTP://[::1]:8081",
            "HTTP://127.0.0.1:0",
        ] {
            one_sentence(&endpoint(spelling).expect_err("a refusal"));
        }
        assert!(endpoint("HTTPS://127.0.0.1:8081").unwrap_err().contains("http://"));
        assert!(endpoint("HTTP://localhost:8081").unwrap_err().contains("127.0.0.1"));
        assert!(endpoint("HTTP://127.0.0.1:0").unwrap_err().contains("port 0"));
    }

    /// A query and a fragment are not part of an origin, and this file's request line is a
    /// constant, so both are refused with the character that caused it — not with the port
    /// or user-name sentence the parse used to fall into.
    #[test]
    fn a_query_or_a_fragment_is_refused_with_a_sentence_that_names_it() {
        for (url, needle) in [
            ("http://127.0.0.1:8081?x=1", "query"),
            ("http://127.0.0.1:8081?", "query"),
            ("http://127.0.0.1:8081/v1/chat/completions?x=1", "query"),
            ("HTTP://127.0.0.1:8081?x=1", "query"),
            ("http://127.0.0.1:8081#@evil", "fragment"),
            ("http://127.0.0.1:8081#", "fragment"),
            ("http://127.0.0.1:8081/#frag", "fragment"),
            // A fragment is named before the authority is read, so the `@` inside it does
            // not turn into a user-name complaint about a user that is not there.
            ("http://127.0.0.1:8081#user:pw@evil", "fragment"),
        ] {
            let problem = endpoint(url).expect_err("a refusal");
            one_sentence(&problem);
            assert!(
                problem.contains(needle),
                "{url} was refused with {problem:?}, which does not name {needle:?}"
            );
            // The byte offset, not the text: the url may carry a control character.
            assert!(problem.contains("byte "), "{url}: {problem:?}");
            assert!(!problem.contains("127.0.0.1:8081?"), "{url}: echoed the value: {problem:?}");
        }
    }

    #[test]
    fn anything_but_http_on_loopback_is_refused_with_a_sentence() {
        for (url, expected) in [
            ("https://127.0.0.1:8081", "http://"),
            ("ftp://127.0.0.1:8081", "http://"),
            ("127.0.0.1:8081", "http://"),
            ("http://localhost:8081", "127.0.0.1"),
            ("http://[::1]:8081", "127.0.0.1"),
            ("http://user:pw@127.0.0.1:8081", "user name"),
            ("http://10.1.2.3:8081", "127.0.0.1"),
            ("http://127.0.0.1", "port"),
            ("http://127.0.0.1:http", "number"),
            ("", "http://"),
            ("http://127.0.0.1:0", "port 0"),
            ("http://127.0.0.1:8081/../..", ".."),
            // The scheme is case-insensitive, but the address rule is not relaxed by it —
            // and only `http` is a scheme this client speaks.
            ("HTTPS://127.0.0.1:8081", "http://"),
            ("HTTP://localhost:8081", "127.0.0.1"),
            ("HTTP://10.1.2.3:8081", "127.0.0.1"),
        ] {
            let problem = endpoint(url).expect_err("a refusal");
            assert!(
                problem.contains(expected),
                "{url} was refused with {problem:?}, which does not name {expected:?}"
            );
            one_sentence(&problem);
        }
    }

    #[test]
    fn the_request_is_one_post_with_a_body_length_in_bytes() {
        let payload = body(
            "be brief",
            &[Message::user("Grüße — how high?")],
            &[],
            Budget::SHIPPED,
        );
        let request = request("127.0.0.1:8081", &payload);
        let (head, sent) = request.split_once("\r\n\r\n").expect("a head and a body");

        // The blank line that opens the body closes the last header line, so the head
        // is every line of the request up to it — one line per header, none doubled.
        let lines: Vec<&str> = head.split("\r\n").collect();
        assert_eq!(lines[0], "POST /v1/chat/completions HTTP/1.1");
        assert!(lines.contains(&"Host: 127.0.0.1:8081"), "{lines:?}");
        assert!(lines.contains(&"Content-Type: application/json"), "{lines:?}");
        assert!(lines.contains(&"Connection: close"), "{lines:?}");
        assert!(
            lines.contains(&format!("Content-Length: {}", payload.len()).as_str()),
            "{lines:?}"
        );
        assert_eq!(sent, payload, "the body follows the blank line unchanged");
        // `Grüße` is one character more than it is a byte more: bytes are what a
        // `Content-Length` counts, and a character count would truncate the body.
        assert!(payload.len() > payload.chars().count());
        assert!(
            head.contains(&format!("Content-Length: {}", payload.len())),
            "the length is the body's byte count:\n{head}"
        );
    }

    #[test]
    fn the_body_is_the_system_prompt_then_the_conversation() {
        let payload = body(
            "answers come from the archive; be brief",
            &[
                Message::user("what happened with chips?\n\"quoted\" \\ and a tab\t"),
                Message::assistant("Exports fell 4%."),
                Message::user("when?"),
            ],
            &[],
            Budget::SHIPPED,
        );
        let value: Value = serde_json::from_str(&payload).expect("the body is JSON");
        // The two assertions this page exists for: the answer streams, and the model is
        // asked to think with the whole budget.
        assert_eq!(value["stream"], json!(true), "frames, not one answer");
        assert_eq!(
            value["max_tokens"],
            json!(Budget::SHIPPED.max_tokens),
            "the body carries the budget it was handed, not a constant of its own"
        );
        assert!(
            THINKING_TOKENS >= 8_192,
            "at least the thinking budget the audit asked for"
        );
        assert_eq!(
            value["chat_template_kwargs"]["enable_thinking"],
            json!(true),
            "the model is asked to think"
        );
        let messages = value["messages"].as_array().expect("a message array");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(
            messages[0]["content"],
            json!("answers come from the archive; be brief")
        );
        assert_eq!(messages[1]["role"], json!("user"));
        assert_eq!(messages[2]["role"], json!("assistant"));
        assert_eq!(messages[3]["content"], json!("when?"));
        // The awkward characters came back out of the JSON exactly as they went in:
        // nothing here tried to escape a string by hand.
        assert_eq!(
            messages[1]["content"],
            json!("what happened with chips?\n\"quoted\" \\ and a tab\t")
        );
    }

    #[test]
    fn a_reply_with_a_content_length_is_read_to_that_length() {
        let payload = "{\"choices\":[{\"index\":0}]}";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}TRAILING",
            payload.len()
        );
        assert!(whole(response.as_bytes()), "the length says the reply is whole");
        let (status, body) = split(response.as_bytes()).expect("a whole reply");
        assert_eq!(status, 200);
        assert_eq!(
            String::from_utf8_lossy(&body),
            payload,
            "the head's length decides where the body stops, so bytes after it are not part of it"
        );
        // One byte short of the length is not a whole reply yet.
        let short = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len() + 1
        );
        assert!(!whole(short.as_bytes()));
    }

    #[test]
    fn a_reply_that_ends_when_the_connection_does_is_not_whole_early() {
        // No length, no chunking: only the close says the body ended, so every read is
        // a "keep reading" — a reply cannot be cut at a frame boundary.
        let partial = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"choices\":";
        assert!(!whole(partial));
        let (status, body) = split(partial).expect("what arrived is the body");
        assert_eq!(status, 200);
        assert_eq!(body, b"{\"choices\":");
    }

    #[test]
    fn a_chunked_reply_is_decoded_frame_by_frame() {
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"ch\r\n5\r\noice\"\r\n0\r\n\r\n";
        assert!(whole(chunked));
        let (status, body) = split(chunked).expect("a decoded reply");
        assert_eq!(status, 200);
        assert_eq!(String::from_utf8_lossy(&body), "{\"choice\"");
        // Half a frame is not a whole reply, and is not handed on as one.
        let cut = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"ch\r\n9\r\n";
        assert!(!whole(cut));
        assert!(split(cut).is_err());
    }

    #[test]
    fn a_head_that_is_not_http_is_a_sentence_not_a_panic() {
        for reply in [b"".as_slice(), b"not http at all\r\n\r\n".as_slice()] {
            let problem = split(reply).expect_err("a refusal");
            one_sentence(&problem);
        }
    }

    #[test]
    fn the_answer_is_the_message_content() {
        let reply = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"  Exports fell 4%.  "}}]}"#;
        assert_eq!(reply_from(reply).expect("an answer").answer, "Exports fell 4%.");
    }

    #[test]
    fn a_refusal_an_empty_answer_and_a_broken_body_are_three_sentences() {
        let refused = reply_from(r#"{"error":{"message":"the request exceeds the available context size, try increasing it"}}"#)
            .expect_err("a refusal");
        assert!(refused.contains("refused the question"), "{refused}");
        assert!(
            refused.contains("the request exceeds the available context size"),
            "the server's own words are kept: {refused}"
        );
        one_sentence(&refused);

        for reply in [
            r#"{"choices":[{"message":{"content":"   "}}]}"#,
            r#"{"choices":[]}"#,
            r#"{}"#,
        ] {
            let empty = finish(
                reply_from(reply).expect("a reply"),
                "at",
                Budget::SHIPPED.max_tokens,
            )
            .expect_err("no answer");
            assert!(empty.contains("answered nothing"), "{empty}");
            one_sentence(&empty);
        }

        let broken = reply_from("<html>a proxy's idea of an answer</html>").expect_err("no JSON");
        assert!(broken.contains("not JSON"), "{broken}");
        one_sentence(&broken);
    }

    #[test]
    fn thinking_and_an_answer_are_two_channels_and_never_one() {
        // A thinking model's non-streamed reply carries `reasoning_content` beside
        // `content`. They are kept apart: a page that showed the working-out as the answer
        // would be lying about what the model said.
        let reply = r#"{"choices":[{"message":{"reasoning_content":"First, chips.","content":"Chips moved."},"finish_reason":"stop"}]}"#;
        let kept = reply_from(reply).expect("a reply");
        assert_eq!(kept.thinking, "First, chips.");
        assert_eq!(kept.answer, "Chips moved.");
        assert!(!kept.cut);
        assert_eq!(kept.thought_chars(), 13);
    }

    #[test]
    fn the_budget_is_the_readers_two_numbers_and_never_a_constant_of_this_files() {
        // (c) Nothing in `meta`: nobody has configured this machine, so the request must
        // be exactly the one this window has always sent — not a new arithmetic answer.
        let fresh = Budget::from_meta(None, None);
        assert_eq!(fresh, Budget::SHIPPED, "{fresh:?}");
        assert_eq!(
            fresh.max_tokens,
            THINKING_TOKENS + DEFAULT_ANSWER,
            "an unconfigured machine gets the defaults every other part of the app uses"
        );
        assert!(fresh.thinking, "a fresh machine thinks, as it always did");
        let wire = body("be brief", &[Message::user("q")], &[], fresh);
        let value: Value = serde_json::from_str(&wire).expect("JSON");
        assert_eq!(value["max_tokens"], json!(THINKING_TOKENS + DEFAULT_ANSWER));
        assert_eq!(value["chat_template_kwargs"]["enable_thinking"], json!(true));

        // (a) `model_reasoning = 0`: the reader turned thinking off, so no thinking key
        // at all is sent — `false` is a different question to a template — and the answer
        // gets its own room and nothing else.
        let reflex = Budget::from_meta(Some("0"), Some("4096"));
        assert!(!reflex.thinking, "{reflex:?}");
        assert_eq!(reflex.max_tokens, 4096);
        let wire = body("be brief", &[Message::user("q")], &[], reflex);
        let value: Value = serde_json::from_str(&wire).expect("JSON");
        assert_eq!(value["max_tokens"], json!(4096));
        assert!(
            value.get("chat_template_kwargs").is_none(),
            "a model with no thinking room is asked to think anyway: {wire}"
        );

        // (b) The reader's own two rooms add up: one `max_tokens` on the wire, because
        // llama.cpp has one budget, and thinking is asked for because there is room.
        let raised = Budget::from_meta(Some("12000"), Some("4000"));
        assert_eq!(
            raised,
            Budget {
                thinking: true,
                max_tokens: 16_000
            }
        );
        let wire = body("be brief", &[Message::user("q")], &[], raised);
        let value: Value = serde_json::from_str(&wire).expect("JSON");
        assert_eq!(value["max_tokens"], json!(16_000));

        // One room raised, the other unset: the shared default answers the second, not 0.
        assert_eq!(
            Budget::from_meta(Some("0"), None),
            Budget {
                thinking: false,
                max_tokens: DEFAULT_ANSWER
            }
        );
        assert_eq!(
            Budget::from_meta(None, Some("2000")),
            Budget {
                thinking: true,
                // The thinking default plus the answer the reader named.
                max_tokens: THINKING_TOKENS + 2_000
            }
        );
        // A value that does not parse is *absent*, never 0 — a typo must not turn thinking
        // off behind the reader's back.
        assert_eq!(Budget::from_meta(Some("lots"), None), Budget::SHIPPED);
        assert_eq!(
            Budget::from_meta(Some("lots"), Some("3000")),
            Budget {
                thinking: true,
                max_tokens: THINKING_TOKENS + 3_000
            }
        );
        // Two enormous rooms are a bad request, not an overflow panic on the drawing
        // thread.
        assert_eq!(
            Budget::from_meta(Some("4294967295"), Some("4294967295")).max_tokens,
            u32::MAX
        );
    }

    #[test]
    fn a_budget_spent_on_thinking_is_a_sentence_that_names_the_budget() {
        // (d) The case the user asked about: the model thought until the budget ran out and
        // never started the answer. The sentence quotes the budget this request *sent* —
        // here 16 000, a number the constant has never been — so a reader who raised the
        // room is not told about 8 192.
        let thought = Reply {
            thinking: "Let me think about chips for a while.".to_string(),
            answer: String::new(),
            cut: true,
            tool_calls: Vec::new(),
        };
        let sentence = finish(thought.clone(), "at", 16_000).expect_err("no answer");
        assert!(sentence.contains("ran out of room while thinking"), "{sentence}");
        assert!(
            sentence.contains("16000-token"),
            "the budget the request sent is named: {sentence}"
        );
        assert!(
            !sentence.contains(&format!("{THINKING_TOKENS}-token")),
            "the sentence quotes the constant, not the budget sent: {sentence}"
        );
        one_sentence(&sentence);

        // On a machine nobody configured it is the shipped number, and the same sentence.
        let sentence = finish(thought, "at", Budget::SHIPPED.max_tokens).expect_err("no answer");
        assert!(
            sentence.contains(&format!("{}-token", Budget::SHIPPED.max_tokens)),
            "{sentence}"
        );
        one_sentence(&sentence);

        // Thinking that stops on its own is a different sentence, and also one sentence.
        let stopped = Reply {
            thinking: "Hm.".to_string(),
            answer: String::new(),
            cut: false,
            tool_calls: Vec::new(),
        };
        let sentence = finish(stopped, "at", 16_000).expect_err("no answer");
        assert!(sentence.contains("thought for 3 characters"), "{sentence}");
        one_sentence(&sentence);
    }

    #[test]
    fn a_frame_is_its_data_lines_and_nothing_else() {
        // The delta a stream carries: one frame, two channels, either may be empty.
        let frame = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"chips \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"Chips\"}}]}\n\ndata: [DONE]\n\n";
        let mut framer = Framer::new(false);
        framer.push(frame.as_bytes());
        let frames = framer.frames().expect("frames");
        assert_eq!(frames.len(), 3);
        match piece(&frames[0]).expect("a delta") {
            Piece::Delta {
                thinking, content, ..
            } => {
                assert_eq!(thinking, "chips ");
                assert!(content.is_empty());
            }
            _ => panic!("the first frame is not a delta"),
        }
        match piece(&frames[1]).expect("a delta") {
            Piece::Delta {
                thinking, content, ..
            } => {
                assert!(thinking.is_empty());
                assert_eq!(content, "Chips");
            }
            _ => panic!("the second frame is not a delta"),
        }
        assert!(matches!(
            piece(&frames[2]).expect("the end"),
            Piece::Done
        ));
    }

    #[test]
    fn a_frame_cut_in_half_waits_for_the_rest_and_then_is_a_sentence() {
        // A read can end anywhere, including inside a JSON frame. Half a frame is not half
        // an answer: it waits for the rest, and a stream that never finishes it is one
        // sentence rather than a partial draw.
        let mut framer = Framer::new(false);
        framer.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Chi");
        assert_eq!(framer.frames().expect("nothing yet").len(), 0);
        assert!(framer.partial(), "half a frame is waiting");
        framer.push(b"ps\"}}]}\n\n");
        let frames = framer.frames().expect("the rest");
        assert_eq!(frames.len(), 1);
        assert!(!framer.partial());
    }

    #[test]
    fn a_chunked_stream_is_peeled_a_chunk_at_a_time() {
        // `llama-server` writes chunks: a size line, bytes, a CRLF. The decoder must not
        // care where a read ends — inside a size line, inside a chunk, or between two.
        // The wire is built from the frame so the sizes are the frame's own.
        let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"Chips\"}}]}\n\n";
        let (head, tail) = frame.split_at(18);
        let wire = format!(
            "{:x}\r\n{head}\r\n{:x}\r\n{tail}\r\n0\r\n\r\n",
            head.len(),
            tail.len()
        );
        let mut framer = Framer::new(true);
        // Ten bytes in: the first size line and the beginning of its chunk.
        framer.push(&wire.as_bytes()[..10]);
        assert!(framer.frames().expect("waiting").is_empty());
        assert!(!framer.partial(), "no frame has started, so nothing is half of one");
        framer.push(&wire.as_bytes()[10..]);
        let frames = framer.frames().expect("the chunk's frame");
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert!(framer.ended, "the zero chunk is the end of the body");
        assert!(!framer.partial());
    }

    #[test]
    fn the_refusal_names_the_address_and_the_command_that_starts_the_server() {
        // This is the sentence a reader sees when the port is not listening, and it is
        // the whole reason the page is usable without a terminal: the command is here.
        let sentence = not_answering("http://127.0.0.1:8081");
        assert!(sentence.contains("http://127.0.0.1:8081"), "{sentence}");
        assert!(sentence.contains(START_COMMAND), "{sentence}");
        assert!(sentence.contains("llama-server"), "{sentence}");
        one_sentence(&sentence);

        // Every other way of failing is one sentence too, and names the address.
        for sentence in [
            silent("http://127.0.0.1:8081", TIMEOUT),
            closed("http://127.0.0.1:8081"),
            too_long("http://127.0.0.1:8081"),
            status_refused("http://127.0.0.1:8081", 500),
        ] {
            assert!(sentence.contains("http://127.0.0.1:8081"), "{sentence}");
            one_sentence(&sentence);
        }
        // The two sentences with no address to name: a reply cut mid-frame, and a worker
        // that ended without one.
        for sentence in [broken_chunk(), NO_ANSWER.to_string()] {
            one_sentence(&sentence);
        }
        // The server's status is in the sentence: "answered HTTP 500" is a fact, and a
        // reader who reports it is reporting something.
        assert!(status_refused("http://127.0.0.1:8081", 500).contains("HTTP 500"));
        // Thirty seconds, in the sentence, is the number the reader waited.
        assert!(silent("http://127.0.0.1:8081", TIMEOUT).contains("30 s"));
        // And a bound the caller chose is the one the sentence names: the wait is read
        // from the run, never from the module constant. A bound below a second is named
        // in milliseconds, because "0 s" is not a wait anybody had.
        assert!(silent("127.0.0.1:8081", Duration::from_millis(120)).contains("120 ms"));
        assert!(!silent("127.0.0.1:8081", Duration::from_millis(120)).contains("0 s"));
        assert!(silent("127.0.0.1:8081", Duration::from_secs(2)).contains("2 s"));
        assert!(!silent("127.0.0.1:8081", Duration::from_secs(2)).contains("2000 ms"));
    }

    #[test]
    fn a_refusal_names_the_address_to_write_and_never_the_value_it_refused() {
        // The url comes from `ATLAS_MODEL_URL`: it can carry a password, a control
        // character, or a directory climb. None of it may reach a sentence — a sentence
        // that echoed its input is a label with somebody's token in it, and a newline in
        // that input is a second row on screen.
        let mut refused = 0usize;
        for url in [
            "http://alice:s3cr3t-token@127.0.0.1:8080",
            "http://alice@127.0.0.1:8080",
            "banana-token-9137",
            "http://127.0.0.1:8080\nX-Injected: 1",
            "http://127.0.0.1:8080\u{0}trailing",
            "\u{202e}http://127.0.0.1:8080",
            "http://127.0.0.1:8081/../..",
        ] {
            let Ok((_, _)) = endpoint(url) else {
                let sentence = endpoint(url).expect_err("a refusal");
                refused += 1;
                for leak in [
                    "s3cr3t-token",
                    "alice",
                    "banana-token-9137",
                    "X-Injected",
                    "\u{202e}",
                    "\0",
                ] {
                    assert!(!sentence.contains(leak), "{url:?} leaked {leak:?}: {sentence}");
                }
                one_sentence(&sentence);
                continue;
            };
        }
        assert_eq!(refused, 7, "each hostile value is refused, not accepted");
    }

    /// A sentence a reader can read: one line, ends with a full stop, and short enough
    /// to sit under a chat box instead of becoming the page.
    fn one_sentence(sentence: &str) {
        assert!(!sentence.is_empty(), "an empty message says nothing");
        assert!(!sentence.contains('\n'), "{sentence:?}");
        assert!(sentence.ends_with('.'), "{sentence:?}");
        assert!(sentence.chars().count() < 260, "{sentence:?}");
        assert!(!sentence.contains("  "), "{sentence:?}");
        // Sentence case: the first character is a capital, not merely a letter. Every
        // sentence here starts with "The"; the rule is pinned so a new one cannot slip in
        // lowercase ("the model address is not …") and read as a continuation.
        assert!(
            sentence.chars().next().is_some_and(char::is_uppercase),
            "not sentence case: {sentence:?}"
        );
    }
}
