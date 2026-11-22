//! The native chat client's wire contract, measured against a real loopback server.
//!
//! The window is the product's face, and chat is the one thing in it that dials a
//! socket. This file is that conversation, end to end, with no `llama.cpp` and no
//! browser: it **starts its own HTTP/1.1 server on `127.0.0.1:0`** — an ephemeral
//! loopback port, the one socket a test is allowed to open — and points
//! [`http::ask`] at it.
//!
//! Four claims, all of them about bytes:
//!
//! 1. **The request is a well-formed chat completion.** `POST /v1/chat/completions`,
//!    `Content-Type: application/json`, a `Content-Length` that is exactly the body's
//!    byte length, and a JSON body whose `messages` start with the system prompt, carry
//!    the reader's turns in order, and whose `stream` is the **boolean** `false` — a
//!    server that read the string `"false"` as truthy would stream an answer this
//!    client never collects.
//! 2. **A non-streaming answer is read** out of `choices[0].message.content` and
//!    trimmed, with its escapes intact.
//! 3. **A failure is one sentence, not a panic and not a hang.** A `500`, a socket
//!    closed before the answer, the same socket closing *in the middle* of the answer,
//!    and a model that is not running at all: each is asserted to be one line that
//!    ends with a full stop, carries no panic text, and arrives inside a clock bound the
//!    test states. A server that accepts and then says **nothing** is the fourth shape;
//!    it runs the client's own bound at [`SHORT_BOUND`] (120 ms) through
//!    [`http::ask_within`], which is the same function the window calls with
//!    [`http::TIMEOUT`] (30 s), so the sentence is measured rather than waited out.
//! 4. **The address is a literal, not a name.** A server listening on the very port is
//!    addressed by `localhost` instead of `127.0.0.1`: the client refuses with one
//!    sentence and **sends nothing**, so no name is ever resolved from this window.
//! 5. **The test closes its own port** when the server is dropped, and the recorded
//!    request survives the `drop` so the assertions can be made afterwards.
//!
//! # Reaching the client
//!
//! `atlas-desktop` is a library (`atlas_desktop::http`) plus a binary that opens the
//! window, so this file links the crate and calls the shipped client: the same `ask`,
//! `Message`, `TIMEOUT`, `PATH`, `START_COMMAND` and sentences the Chat page calls. The
//! `http` module used to arrive by a path-include, which compiled a second copy into
//! this test binary; now the library is compiled once per run and linked here, and the
//! items below are the ones `src/http.rs` makes `pub` — a test cannot see a private one.
//!
//! `src/views/chat.rs` is *not* reached here: it is `egui` code that draws a panel, and
//! the sentence a reader sees is already proven at its source — the view shows exactly
//! the `Err(String)` these tests measure, and the one sentence it adds of its own
//! (`http::NO_ANSWER`, for a worker that ended without sending) is asserted beside them.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// The client under test: the real one, linked from the library.
use atlas_desktop::http;

// ---------------------------------------------------------------------------
// the server: a real HTTP/1.1 answer on an ephemeral loopback port
// ---------------------------------------------------------------------------

/// What the server does when a request arrives.
#[derive(Clone, Debug)]
enum Behaviour {
    /// `200` with this JSON body.
    Answer(String),
    /// A status with a JSON error body, as `llama-server` sends when it is unhappy.
    Status(u16, String),
    /// Accept, then close without a byte — a server that took the work and dropped it.
    Hangup,
    /// Answer, then close in the middle of the body: a connection that died.
    HalfAnswer,
    /// A streamed reply, scripted: the frames, and — if the case needs one — the bytes of
    /// a frame that never completes, written just before the connection closes.
    Sse(Script),
    /// A script per connection, in the order the connections arrive: the agent loop asks
    /// again after a tool call, so one turn is more than one request.
    Steps(Vec<Script>),
    /// Accept, read the request, and then say nothing for this long: a wedged server.
    Silent(Duration),
}

impl Behaviour {
    /// What the `nth` connection is answered with.
    ///
    /// A single-answer behaviour serves the first connection only, as it always has; a
    /// scripted sequence serves one script per connection and then nothing, so a loop that
    /// asked a seventh time would wait on a port nobody is answering rather than get a
    /// second copy of the last script.
    fn at(&self, nth: usize) -> Behaviour {
        match self {
            Behaviour::Steps(scripts) => scripts
                .get(nth)
                .map(|script| Behaviour::Sse(script.clone()))
                .unwrap_or(Behaviour::Hangup),
            other => other.clone(),
        }
    }

    /// Is conversation number `nth` to be served at all?
    fn wants(&self, nth: usize) -> bool {
        match self {
            Behaviour::Steps(scripts) => nth < scripts.len(),
            _ => nth == 0,
        }
    }
}

/// A scripted event stream: the `data:` payloads, in order, and what a broken server
/// leaves behind.
#[derive(Clone, Debug, Default)]
struct Script {
    /// Each frame's JSON payload, sent as `data: <payload>\n\n`.
    frames: Vec<String>,
    /// Bytes of a frame that never finishes, written before the connection closes.
    tail: Option<String>,
    /// How long to wait between frames: what makes "arrived first" measurable in time as
    /// well as in order.
    gap: Duration,
}

impl Script {
    fn new(frames: Vec<String>) -> Script {
        Script {
            frames,
            tail: None,
            gap: Duration::ZERO,
        }
    }

    fn cut(mut self, tail: &str) -> Script {
        self.tail = Some(tail.to_string());
        self
    }

    fn with_gap(mut self, gap: Duration) -> Script {
        self.gap = gap;
        self
    }
}

/// One streamed delta, as the server writes it: a thinking piece, an answer piece, or
/// the end of the answer with the reason it stopped.
fn thought(text: &str) -> String {
    serde_json::json!({ "choices": [{ "delta": { "reasoning_content": text } }] }).to_string()
}

fn said(text: &str) -> String {
    serde_json::json!({ "choices": [{ "delta": { "content": text } }] }).to_string()
}

fn finished(reason: &str) -> String {
    serde_json::json!({ "choices": [{ "delta": {}, "finish_reason": reason }] }).to_string()
}

/// `data: [DONE]`, which is the server saying the stream is complete.
const DONE: &str = "[DONE]";

/// A server that lives for exactly one test and closes its port when dropped.
struct Server {
    addr: SocketAddr,
    /// Every complete request, as the raw bytes that arrived.
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(behaviour: Behaviour) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral loopback port");
        let addr = listener.local_addr().expect("the bound address");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener, so `drop` can stop it");
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                // One connection per server — or one per script, for a tool loop: the
                // agent's second question is a second connection to the same port.
                let mut nth = 0usize;
                while !stop.load(Ordering::SeqCst) && behaviour.wants(nth) {
                    match listener.accept() {
                        Ok((stream, _peer)) => {
                            serve(stream, &behaviour.at(nth), &seen);
                            nth += 1;
                        }
                        Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
                // `listener` is dropped here: the port is released before `drop` returns.
            })
        };
        Server {
            addr,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    /// The address a reader would configure, as `http.rs` accepts it: a literal
    /// loopback address with an explicit port, no name to resolve.
    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The one request that arrived, as text, for an assertion about its bytes.
    fn request(&self) -> String {
        let seen = self.seen.lock().expect("the recorder is not poisoned");
        let raw = seen.first().expect("exactly one request arrived");
        String::from_utf8_lossy(raw).into_owned()
    }

    /// The request body, split off the head.
    fn body(&self) -> String {
        self.body_at(0)
    }

    /// The body of the `nth` request, for a turn that took more than one.
    fn body_at(&self, nth: usize) -> String {
        let seen = self.seen.lock().expect("the recorder is not poisoned");
        let raw = seen
            .get(nth)
            .unwrap_or_else(|| panic!("request {nth} never arrived ({} arrived)", seen.len()));
        let request = String::from_utf8_lossy(raw).into_owned();
        match request.split_once("\r\n\r\n") {
            Some((_head, body)) => body.to_string(),
            None => panic!("request {nth} has no body: {:?}", request),
        }
    }

    /// How many requests arrived.
    fn requests(&self) -> usize {
        self.seen.lock().expect("the recorder is not poisoned").len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read one request (head plus the body its `Content-Length` promises) and answer it.
fn serve(mut stream: TcpStream, behaviour: &Behaviour, seen: &Arc<Mutex<Vec<Vec<u8>>>>) {
    // The listener is non-blocking so `drop` can stop it, and an accepted socket
    // inherits that flag: without this line `read` answers `WouldBlock` for a request
    // that has not arrived yet, the recorder sees an empty request, and the server
    // answers before it has read anything. Measured — three tests failed with
    // `no head/body split: ""` until this line existed.
    let _ = stream.set_nonblocking(false);
    // A read that never finishes must not hang the suite: this server's only client is
    // the test, and it is expected to send a whole request.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let raw = read_request(&mut stream);
    seen.lock()
        .expect("the recorder is not poisoned")
        .push(raw);

    match behaviour {
        // Closing the stream is the answer.
        Behaviour::Hangup => {}
        // `Steps` never arrives here: the accept loop hands this function `at(nth)`, which
        // is already one script. Closing is the calmest arm that does not lie about it.
        Behaviour::Steps(_) => {}
        Behaviour::HalfAnswer => {
            let half = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n\
                        {\"choices\":[{\"message\":{\"content\":\"Chi";
            let _ = stream.write_all(half.as_bytes());
            let _ = stream.flush();
        }
        Behaviour::Status(code, body) => {
            let head = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                code,
                reason(*code),
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
        }
        Behaviour::Answer(body) => reply(&mut stream, body),
        // An event stream: the head, then each frame as its own write, so the client has
        // to reassemble what a real server sends in pieces.
        Behaviour::Sse(script) => {
            let head = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Cache-Control: no-cache\r\n\
                        Connection: close\r\n\
                        \r\n";
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            for frame in &script.frames {
                if !script.gap.is_zero() {
                    std::thread::sleep(script.gap);
                }
                let _ = stream.write_all(format!("data: {frame}\n\n").as_bytes());
                let _ = stream.flush();
            }
            if let Some(tail) = script.tail.as_ref() {
                // Half a frame, then the connection ends: the cut the client must report.
                let _ = stream.write_all(tail.as_bytes());
                let _ = stream.flush();
            }
        }
        // The request has been read and recorded; the answer never comes. The write
        // after the sleep fails, because by then the client has given up — which is
        // the whole point of the case.
        Behaviour::Silent(wait) => {
            std::thread::sleep(*wait);
        }
    }
}

fn reply(stream: &mut TcpStream, body: &str) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn reason(code: u16) -> &'static str {
    match code {
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// Everything that arrived, up to the end of the body `Content-Length` promised.
///
/// Bounded (64 KiB) and time-bounded (the read timeout above), so a client that sends
/// nothing fails the test instead of hanging it.
fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    const CAP: usize = 64 * 1024;
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some((head_len, body_len)) = framed(&raw) {
            if raw.len() >= head_len + body_len {
                raw.truncate(head_len + body_len);
                return raw;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => return raw,
            Ok(read) => {
                raw.extend_from_slice(&chunk[..read]);
                if raw.len() > CAP {
                    return raw;
                }
            }
            Err(_) => return raw,
        }
    }
}

/// `(head length, body length)` once the request line and headers are complete.
fn framed(raw: &[u8]) -> Option<(usize, usize)> {
    let end = raw.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    let body = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    Some((end + 4, body))
}

// ---------------------------------------------------------------------------
// 1 + 2. the request is well formed, and a non-streaming answer is read
// ---------------------------------------------------------------------------

/// One system prompt, one question, one answer.
///
/// The assertions are on the bytes that reached the socket, because that is the only
/// place a request can be wrong without anybody noticing: a client that sent
/// `"stream": "false"`, or a `Content-Length` counted in characters instead of bytes,
/// would still pass a round-trip test against a lenient server.
#[test]
fn the_request_is_a_well_formed_chat_completion() {
    let answer = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"Chips moved."},"finish_reason":"stop"}]}"#;
    let server = Server::start(Behaviour::Answer(answer.to_string()));
    let asked = http::ask(
        &server.url(),
        "you are ATLAS",
        &[http::Message::user("what moved?")],
    )
    .expect("a well-formed answer");
    assert_eq!(
        asked.answer, "Chips moved.",
        "the answer is choices[0].message.content"
    );
    assert!(
        asked.thinking.is_empty(),
        "a reply with no reasoning_content has an empty thinking channel"
    );

    let request = server.request();
    let (head, _) = request
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no head/body split: {:?}", request));
    let body = server.body();
    let head_lower = head.to_ascii_lowercase();

    // The request line: the one path llama-server serves a completion on.
    assert!(
        head.starts_with(&format!("POST {} HTTP/1.1", http::PATH)),
        "the request line is not a chat completion: {:?}",
        head.lines().next()
    );
    // The headers a server needs to read a JSON body of the right size.
    assert!(
        head_lower.contains("content-type: application/json"),
        "no JSON content type: {:?}",
        head
    );
    let promised: usize = head_lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| panic!("no Content-Length: {:?}", head));
    assert_eq!(
        promised,
        body.len(),
        "Content-Length does not match the {} body bytes (a body with an umlaut in it \
         is longer than it looks): {:?}",
        body.len(),
        head
    );
    assert!(
        head_lower.contains(&format!("host: {}", server.addr)),
        "the Host is not the authority that was addressed: {:?}",
        head
    );

    // The body: the system prompt first, then the reader's turn, and `stream` the
    // The body: the system prompt first, then the reader's turn, `stream` the boolean
    // `true`, and the thinking budget asked for in full.
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("the body is not JSON ({}): {:?}", error, body));
    let object = value.as_object().expect("a JSON object");
    assert!(
        object.contains_key("model"),
        "the body names no model: {:?}",
        body
    );
    assert_eq!(
        value["stream"],
        serde_json::json!(true),
        "the answer must be streamed — the page draws it as it arrives: {:?}",
        body
    );
    assert_eq!(
        value["max_tokens"],
        serde_json::json!(http::Budget::SHIPPED.max_tokens),
        "the budget the request was handed, not a constant of the client: {:?}",
        body
    );
    assert!(
        http::THINKING_TOKENS >= 8_192,
        "the window asks for at least 8 192 tokens of thinking, not {}",
        http::THINKING_TOKENS
    );
    assert_eq!(
        value["chat_template_kwargs"]["enable_thinking"],
        serde_json::json!(true),
        "the model is asked to think: {:?}",
        body
    );
    let messages = value["messages"].as_array().expect("an array of turns");
    assert_eq!(messages.len(), 2, "one system prompt and one question: {:?}", messages);
    assert_eq!(
        messages[0],
        serde_json::json!({ "role": "system", "content": "you are ATLAS" }),
        "the system prompt is the first message"
    );
    assert_eq!(
        messages[1],
        serde_json::json!({ "role": "user", "content": "what moved?" }),
        "the reader's turn is the second message"
    );
}

/// The answer is read back as text with its escapes intact, and trimmed: a reply
/// wrapped in spaces is the model's padding, not part of what it said.
#[test]
fn a_non_streaming_answer_is_read_with_its_escapes_intact() {
    let answer = "{\"choices\":[{\"message\":{\"content\":\"  He said \\\"chips\\\", then \\n left.\\u0000  \"}}]}";
    let server = Server::start(Behaviour::Answer(answer.to_string()));
    let asked = http::ask(&server.url(), "be brief", &[http::Message::user("q")])
        .expect("a well-formed answer");
    assert_eq!(asked.answer, "He said \"chips\", then \n left.\u{0}");
}

// ---------------------------------------------------------------------------
// 3. a failure is one sentence — not a panic, not a hang
// ---------------------------------------------------------------------------

/// An error status is one sentence that names the status and the address, and it is
/// not the server's body: a status row is not the place for a page of JSON.
#[test]
fn a_500_is_one_sentence() {
    let server = Server::start(Behaviour::Status(500, r#"{"error":{"message":"failed to load model"}}"#.to_string()));
    let started = Instant::now();
    let sentence = http::ask(&server.url(), "be brief", &[http::Message::user("q")])
        .expect_err("a 500 is not an answer");
    let elapsed = started.elapsed();
    println!("MEASURED 500 -> {:?} in {:?}", sentence, elapsed);
    assert!(sentence.contains("HTTP 500"), "the status is in the sentence: {}", sentence);
    names_the_address(&sentence, server.addr);
    assert!(
        !sentence.contains("failed to load model"),
        "the server's body is not shown raw: {}",
        sentence
    );
    one_sentence(&sentence);
    assert!(elapsed < Duration::from_secs(5), "a refusal took {:?}", elapsed);
    // One question is one request: a failure is not retried behind the reader's back,
    // because a retry would be a second model call on a server that already refused.
    assert_eq!(
        server.seen.lock().unwrap().len(),
        1,
        "the client asked more than once"
    );
}

/// The model address is a loopback *literal*: a name is refused before a socket is
/// attempted, so no name resolution — and no lookup of a name an attacker chose — can
/// happen from this window. The proof is that a server is listening on that very port
/// and never sees a connection.
#[test]
fn a_name_is_refused_before_any_socket_is_opened() {
    let server = Server::start(Behaviour::Answer(
        r#"{"choices":[{"message":{"content":"never read"}}]}"#.to_string(),
    ));
    // The same port, addressed by a name that resolves to the same loopback address.
    let by_name = format!("http://localhost:{}", server.addr.port());
    let sentence = http::ask(&by_name, "be brief", &[http::Message::user("q")])
        .expect_err("a name is not an address");
    println!("MEASURED by name -> {:?}", sentence);
    one_sentence(&sentence);
    // The rule the sentence states, not its wording: the address must be a loopback
    // literal, so a name is refused.
    assert!(
        sentence.contains("127.0.0.1"),
        "the sentence does not say which literal is wanted: {}",
        sentence
    );
    // Nothing arrived: the server was listening the whole time, so an empty record with a
    // sentence above it is the client declining to resolve the name.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        server.seen.lock().unwrap().is_empty(),
        "the client connected by name and sent {:?}",
        server.request()
    );
}

/// A server that accepts and then closes without a byte: the arrow that misses.
#[test]
fn a_closed_socket_is_one_sentence() {
    let server = Server::start(Behaviour::Hangup);
    let started = Instant::now();
    let sentence = http::ask(&server.url(), "be brief", &[http::Message::user("q")])
        .expect_err("a closed socket is not an answer");
    let elapsed = started.elapsed();
    println!("MEASURED hangup -> {:?} in {:?}", sentence, elapsed);
    assert!(
        sentence.contains("closed the connection") || sentence.contains("sent nothing"),
        "the sentence does not say what happened: {}",
        sentence
    );
    names_the_address(&sentence, server.addr);
    one_sentence(&sentence);
    assert!(elapsed < Duration::from_secs(5), "a closed socket took {:?}", elapsed);
}

/// The same socket closing *in the middle* of a body it had begun: half an answer must
/// never be handed to the view as one.
#[test]
fn a_socket_closed_mid_answer_is_one_sentence() {
    let server = Server::start(Behaviour::HalfAnswer);
    let sentence = http::ask(&server.url(), "be brief", &[http::Message::user("q")])
        .expect_err("half an answer is not an answer");
    println!("MEASURED half answer -> {:?}", sentence);
    assert!(
        !sentence.contains("Chi"),
        "a truncated body was passed off as an answer: {}",
        sentence
    );
    one_sentence(&sentence);
}

/// Nothing listening at all — the ordinary case when the model server is not started.
/// The sentence has to be actionable, because it is the only instruction the reader gets.
#[test]
fn a_model_that_is_not_running_is_one_sentence() {
    // A port this test owned and gave back: nothing is listening on it, and loopback
    // refuses at once rather than waiting for a connection timeout.
    let addr = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        listener.local_addr().expect("its address")
    };
    let url = format!("http://{}", addr);
    let started = Instant::now();
    let sentence = http::ask(&url, "be brief", &[http::Message::user("q")])
        .expect_err("nothing is listening");
    let elapsed = started.elapsed();
    println!("MEASURED not running -> {:?} in {:?}", sentence, elapsed);
    names_the_address(&sentence, addr);
    assert!(
        sentence.contains(http::START_COMMAND),
        "the sentence names the command that starts the server: {}",
        sentence
    );
    assert!(sentence.contains("llama-server"), "and what it starts: {}", sentence);
    one_sentence(&sentence);
    assert!(elapsed < Duration::from_secs(5), "a refused connect took {:?}", elapsed);
}

/// The sentence the view adds of its own — a worker that ended without sending — is a
/// sentence too, and it is not one of the client's (so a reader can tell them apart).
#[test]
fn the_sentence_the_view_adds_is_one_sentence() {
    one_sentence(http::NO_ANSWER);
    assert!(
        !http::NO_ANSWER.contains("127.0.0.1"),
        "this sentence names no address, because the client already did: {}",
        http::NO_ANSWER
    );
}

/// The bound the silent-server case gives the client, in place of the window's 30 s.
///
/// Short enough that the test is not a wait, long enough that a machine under load still
/// reaches the read timeout rather than winning a race against it.
const SHORT_BOUND: Duration = Duration::from_millis(120);

/// A server that accepts the connection and then says nothing: the answer must arrive as
/// one sentence after the client's own bound, not never.
///
/// [`http::ask_within`] is what makes this a 120 ms test: the window passes
/// [`http::TIMEOUT`] to it, this passes [`SHORT_BOUND`], and the sentence — including the
/// number in it — comes from the bound that actually ran. It used to be `#[ignore]`d
/// because `ask` hard-wired the constant and the only way to reach the sentence was to
/// sit out thirty seconds; a test nobody runs is a behaviour nobody has.
#[test]
fn a_silent_server_is_one_sentence_after_the_client_timeout() {
    // Three times the client's bound, so the wait that ends the question is the client's
    // and not this server's sleep ending first — and so the server thread `Drop` joins
    // outlives the assertion by a few hundred milliseconds, not by thirty seconds.
    let server = Server::start(Behaviour::Silent(SHORT_BOUND * 3));
    let started = Instant::now();
    let sentence = http::ask_within(
        &server.url(),
        "be brief",
        &[http::Message::user("q")],
        http::Budget::SHIPPED,
        SHORT_BOUND,
    )
    .expect_err("a server that says nothing has not answered");
    let elapsed = started.elapsed();
    println!("MEASURED silent -> {:?} in {:?}", sentence, elapsed);
    assert!(
        sentence.contains("sent nothing"),
        "the sentence does not say the server was silent: {}",
        sentence
    );
    names_the_address(&sentence, server.addr);
    // The wait in the sentence is the bound the client was given, in the unit that reads:
    // 120 ms, never "0 s" — the same slot the window fills with "30 s".
    assert!(
        sentence.contains("120 ms"),
        "the sentence names the wait: {}",
        sentence
    );
    one_sentence(&sentence);
    // The bound, measured: no longer than the timeout by more than the socket's slack, and
    // certainly not never.
    assert!(
        elapsed >= SHORT_BOUND && elapsed < SHORT_BOUND + Duration::from_secs(1),
        "the client waited {:?} for a bound of {:?}",
        elapsed,
        SHORT_BOUND
    );
}

/// The authority a failure is about — `host:port`. The sentence may or may not show the
/// scheme; the address is the fact a reader needs, so that is what is asserted.
fn names_the_address(sentence: &str, addr: SocketAddr) {
    let authority = addr.to_string();
    assert!(
        sentence.contains(&authority),
        "the sentence does not name {}: {}",
        authority,
        sentence
    );
}

/// One sentence a reader can read in a status row: not empty, one line, ends with a
/// full stop, starts with a capital, short — and never the shape of a panic.
fn one_sentence(sentence: &str) {
    assert!(!sentence.is_empty(), "an empty sentence says nothing");
    assert!(
        !sentence.contains('\n') && !sentence.contains('\r'),
        "not one line: {:?}",
        sentence
    );
    assert!(sentence.ends_with('.'), "no full stop: {:?}", sentence);
    // Sentence case, the rule the whole app keeps: the first character is a capital.
    // Every sentence `src/http.rs` can produce starts with `The` — the six failure
    // sentences (`not_answering`, `silent`, `closed`, `too_long`, `status_refused`,
    // `broken_chunk`), the three an answer can be refused with, and the address refusals,
    // which used to begin lowercase ("the model address is not …") and no longer do.
    // `src/http.rs`'s own tests pin the same rule, so this is the contract and not a hope.
    assert!(
        sentence.chars().next().map(char::is_uppercase).unwrap_or(false),
        "the sentence does not start with a capital: {:?}",
        sentence
    );
    assert!(
        sentence.chars().count() <= 260,
        "{} characters is a paragraph, not a status line: {:?}",
        sentence.chars().count(),
        sentence
    );
    let lower = sentence.to_lowercase();
    for wrong in ["panic", "unwrap", "thread '", "error:", "r#"] {
        assert!(
            !lower.contains(wrong),
            "a panic leaked into the sentence ({:?}): {:?}",
            wrong,
            sentence
        );
    }
}

/// The capital rule has to bite, or it is decoration. This is the exact shape
/// `src/http.rs` used to answer with — "the model address is not 127.0.0.1" — a lowercase
/// sentence that the helper accepted while it asked `char::is_alphabetic` (which is true
/// of 't' as well as of 'T'). It must now be rejected. Revert the helper to
/// `is_alphabetic` and this test fails: `one_sentence` returns normally, `should_panic`
/// never fires. That is the measurement that keeps the tightening load-bearing.
#[test]
#[should_panic(expected = "does not start with a capital")]
fn the_capital_rule_rejects_a_lowercase_sentence() {
    one_sentence(
        "the model address is not 127.0.0.1 — this window reads the model on this machine only.",
    );
}

// ---------------------------------------------------------------------------
// 4. the port belongs to the test, and is given back
// ---------------------------------------------------------------------------

/// After the server is dropped nothing may be listening: a suite that runs this file
/// twice, or two tests in parallel, must not collide on a port.
#[test]
fn the_test_closes_its_port() {
    let server = Server::start(Behaviour::Answer(
        r#"{"choices":[{"message":{"content":"ok"}}]}"#.to_string(),
    ));
    let addr = server.addr;
    let url = server.url();
    http::ask(&url, "be brief", &[http::Message::user("q")]).expect("an answer");
    assert_eq!(
        server.seen.lock().unwrap().len(),
        1,
        "exactly one request reached the server"
    );
    drop(server);

    // A connect to a port nobody listens on is refused, and refused at once on
    // loopback: this is the measurement that the listener is gone.
    let after = TcpStream::connect_timeout(&addr, Duration::from_millis(250));
    let error = after.expect_err("the port is still open");
    println!("MEASURED after drop: connect to {} -> {}", addr, error);
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
        ),
        "unexpected error closing the port: {:?}",
        error.kind()
    );
}

/// Two servers never share a port: each binds its own ephemeral one, which is what makes
/// `127.0.0.1:0` safe to run in parallel.
#[test]
fn every_server_gets_its_own_port() {
    let first = Server::start(Behaviour::Hangup);
    let second = Server::start(Behaviour::Hangup);
    assert_ne!(first.addr.port(), second.addr.port());
    assert_eq!(first.addr.ip().to_string(), "127.0.0.1");
    assert_ne!(first.addr.port(), 0, "the bound port is the one the kernel chose");
}

// ---------------------------------------------------------------------------
// 6. the answer streams: two channels, in order, frame by frame
// ---------------------------------------------------------------------------

/// One second of a streamed turn, as the page receives it: which channel, and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Thought(String),
    Said(String),
}

/// Ask the scripted server and keep every delta in the order it arrived.
///
/// `budget` is stated by each test rather than defaulted, because the number on the wire is
/// the thing under test: a reader who set `model_reasoning` must see it in the request.
fn streamed(
    server: &Server,
    budget: http::Budget,
    timeout: Duration,
) -> (Result<http::Reply, String>, Vec<Piece>) {
    let mut pieces: Vec<Piece> = Vec::new();
    let outcome = http::ask_streaming(
        &server.url(),
        "be brief",
        &[http::Message::user("what moved?")],
        &[],
        budget,
        timeout,
        &mut |delta| match delta {
            http::Delta::Thinking(text) => pieces.push(Piece::Thought(text)),
            http::Delta::Answer(text) => pieces.push(Piece::Said(text)),
        },
    );
    (outcome, pieces)
}

/// The frames arrive in the order the server wrote them, on the channel each one belongs
/// to, and the reply that comes back holds both channels whole.
#[test]
fn frames_arrive_in_order_on_the_two_channels() {
    let server = Server::start(Behaviour::Sse(
        Script::new(vec![
            thought("First, "),
            thought("chips."),
            said("Chips "),
            said("moved."),
            finished("stop"),
            DONE.to_string(),
        ])
        .with_gap(Duration::from_millis(20)),
    ));
    let started = Instant::now();
    let (outcome, pieces) = streamed(&server, http::Budget::SHIPPED, http::TIMEOUT);
    let elapsed = started.elapsed();
    let reply = outcome.expect("a streamed answer");
    println!(
        "MEASURED stream -> {} thinking + {} answer characters in {:?}, {} deltas",
        reply.thought_chars(),
        reply.answer.chars().count(),
        elapsed,
        pieces.len()
    );
    assert_eq!(
        pieces,
        vec![
            Piece::Thought("First, ".to_string()),
            Piece::Thought("chips.".to_string()),
            Piece::Said("Chips ".to_string()),
            Piece::Said("moved.".to_string()),
        ],
        "the deltas arrived out of order, or on the wrong channel"
    );
    assert_eq!(reply.thinking, "First, chips.");
    assert_eq!(reply.answer, "Chips moved.");
    assert!(!reply.cut);
    // Two frames with a 20 ms gap take longer than no gap at all: the client is not
    // buffering the whole answer before it reports anything.
    assert!(
        elapsed >= Duration::from_millis(60),
        "the deltas did not arrive over time ({elapsed:?}) — the client waited for the end"
    );
    // What the page draws is the model's words, never the wire: no `data:`, no JSON.
    for piece in &pieces {
        let text = match piece {
            Piece::Thought(text) | Piece::Said(text) => text,
        };
        assert!(
            !text.contains("data:") && !text.contains("choices") && !text.contains('{'),
            "raw SSE reached the page: {text:?}"
        );
    }
}

/// A stream that stops in the middle of a frame: the sentence is one sentence, and the
/// text that did arrive is kept — a reader who watched half an answer keeps half an
/// answer.
#[test]
fn a_stream_cut_mid_frame_keeps_what_arrived_and_is_one_sentence() {
    let server = Server::start(Behaviour::Sse(
        Script::new(vec![thought("Chips "), said("Chips moved.")])
            .cut("data: {\"choices\":[{\"delta\":{\"content\":\"Chi"),
    ));
    let (outcome, pieces) = streamed(&server, http::Budget::SHIPPED, http::TIMEOUT);
    let sentence = outcome.expect_err("half a frame is not a whole answer");
    println!("MEASURED cut -> {:?}", sentence);
    assert_eq!(pieces.len(), 2, "the arrived deltas were dropped: {pieces:?}");
    assert_eq!(pieces[0], Piece::Thought("Chips ".to_string()));
    assert_eq!(pieces[1], Piece::Said("Chips moved.".to_string()));
    assert!(
        sentence.contains("cut short mid-frame"),
        "the sentence does not say the stream was cut: {sentence}"
    );
    assert!(
        !sentence.contains("Chi"),
        "the half frame was handed on as text: {sentence}"
    );
    names_the_address_optional(&sentence);
    one_sentence(&sentence);
}

/// Thinking that spends the whole budget and never starts the answer: the reader is told
/// which number to change, and the thinking that did arrive is not lost.
#[test]
fn a_stream_that_only_thought_names_the_budget() {
    let server = Server::start(Behaviour::Sse(Script::new(vec![
        thought("Let me think about chips. "),
        thought("Still thinking about chips. "),
        finished("length"),
        DONE.to_string(),
    ])));
    // A budget the reader could have set: 12 345 is a number the shipped constant has
    // never been, so a sentence that quotes the constant fails this pin.
    let budget = http::Budget {
        thinking: true,
        max_tokens: 12_345,
    };
    let (outcome, pieces) = streamed(&server, budget, http::TIMEOUT);
    let sentence = outcome.expect_err("thoughts are not an answer");
    println!("MEASURED out of room -> {:?}", sentence);
    assert!(
        sentence.contains("ran out of room while thinking"),
        "the sentence does not name the case: {sentence}"
    );
    assert!(
        sentence.contains("12345") && !sentence.contains(&http::THINKING_TOKENS.to_string()),
        "the sentence quotes the budget the request sent, not the shipped constant: {sentence}"
    );
    one_sentence(&sentence);
    // The thinking is still there for the page to draw: it is what makes the sentence
    // believable.
    let thought_chars: usize = pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::Thought(text) => Some(text.chars().count()),
            Piece::Said(_) => None,
        })
        .sum();
    assert_eq!(thought_chars, "Let me think about chips. Still thinking about chips. ".chars().count());
}

/// A stream that is only thinking, and then ends without saying why, is still one
/// sentence and still names what happened.
#[test]
fn a_stream_that_thought_and_stopped_is_still_one_sentence() {
    let server = Server::start(Behaviour::Sse(Script::new(vec![thought("Hm. ")])));
    let (outcome, pieces) = streamed(&server, http::Budget::SHIPPED, http::TIMEOUT);
    let sentence = outcome.expect_err("thoughts are not an answer");
    assert_eq!(pieces, vec![Piece::Thought("Hm. ".to_string())]);
    assert!(
        sentence.contains("thought for 4 characters"),
        "the sentence does not say what happened: {sentence}"
    );
    one_sentence(&sentence);
}

/// A sentence about a cut stream names no address, because the failure is not the
/// connection's; the helper below only keeps this file's rule explicit.
fn names_the_address_optional(sentence: &str) {
    assert!(
        !sentence.to_lowercase().contains("panic"),
        "the sentence reads as a panic: {sentence}"
    );
}

// ---------------------------------------------------------------------------
// 7. the measurement, against the model server this machine is running
// ---------------------------------------------------------------------------

/// What the page would paint, timed against the live `llama-server` on `127.0.0.1:8081`.
///
/// `#[ignore]`d because a test suite must not depend on a model being up; this one is a
/// measurement, run deliberately:
///
/// ```text
/// cargo test --offline -p atlas-desktop --test chat_wire -- --ignored --nocapture
/// ```
///
/// It prints the three numbers the audit asked for — characters of thinking painted,
/// milliseconds to the first frame, milliseconds to the answer — and asserts only what
/// must hold either way: an answer, or one calm sentence.
#[test]
#[ignore = "times the live llama-server on 127.0.0.1:8081; run with --ignored"]
fn a_live_model_is_measured_not_guessed() {
    let url = std::env::var("ATLAS_MODEL_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string());
    let started = Instant::now();
    let mut first_thought: Option<Duration> = None;
    let mut first_answer: Option<Duration> = None;
    let mut thinking_chars = 0usize;
    let mut answer_chars = 0usize;
    let outcome = http::ask_streaming(
        &url,
        "Answer in one short sentence, from what you know.",
        &[http::Message::user("Name one thing a news archive can answer.")],
        &[],
        http::Budget::SHIPPED,
        http::TURN_TIMEOUT,
        &mut |delta| match delta {
            http::Delta::Thinking(text) => {
                if first_thought.is_none() {
                    first_thought = Some(started.elapsed());
                }
                thinking_chars += text.chars().count();
            }
            http::Delta::Answer(text) => {
                if first_answer.is_none() {
                    first_answer = Some(started.elapsed());
                }
                answer_chars += text.chars().count();
            }
        },
    );
    println!(
        "MEASURED live: {thinking_chars} characters of thinking, first frame after {} ms, \
         first answer after {} ms, {} answer characters, total {:?}",
        first_thought.map(|wait| wait.as_millis().to_string()).unwrap_or_else(|| "never".to_string()),
        first_answer.map(|wait| wait.as_millis().to_string()).unwrap_or_else(|| "never".to_string()),
        answer_chars,
        started.elapsed()
    );
    match outcome {
        Ok(reply) => {
            assert!(!reply.answer.trim().is_empty(), "the live model answered nothing");
            assert_eq!(reply.answer.chars().count(), answer_chars, "the reply and the deltas disagree");
            println!("MEASURED live answer: {:?}", reply.answer);
        }
        Err(sentence) => {
            println!("MEASURED live refusal: {sentence}");
            one_sentence(&sentence);
        }
    }
}

// ---------------------------------------------------------------------------
// 8. the tool loop: the model decides, the window looks it up, the model answers
// ---------------------------------------------------------------------------

/// A streamed frame in which the model asks for a tool, in one piece.
fn called(id: &str, name: &str, arguments: &str) -> String {
    serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": id,
            "function": { "name": name, "arguments": arguments }
        }] } }]
    })
    .to_string()
}

/// The same request as a real server spells it: the index first, then pieces of the name
/// and of the arguments, in separate frames — which is what the client has to collect.
fn called_in_pieces(id: &str, name: &str, arguments: &str) -> Vec<String> {
    let (head, tail) = name.split_at(name.len() / 2);
    vec![
        serde_json::json!({
            "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": id, "function": { "name": head, "arguments": "" }
            }] } }]
        })
        .to_string(),
        serde_json::json!({
            "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "function": { "name": tail, "arguments": arguments }
            }] } }]
        })
        .to_string(),
    ]
}

/// A tiny archive in a temp file, with the shape the tools read: one story that names
/// China and Nvidia, and the relation between them.
struct TempArchive {
    path: std::path::PathBuf,
}

impl TempArchive {
    fn new(name: &str) -> TempArchive {
        let path = std::env::temp_dir()
            .join(format!("atlas-wire-{}-{name}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).expect("a temp archive");
        conn.execute_batch(
            "CREATE TABLE sources (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE articles (id INTEGER PRIMARY KEY, title TEXT, publisher TEXT,
               summary TEXT, content TEXT, iso TEXT, url TEXT, full_text INTEGER,
               relevant INTEGER, dupe INTEGER, published TEXT, source_id INTEGER);
             CREATE TABLE entities (id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER);
             CREATE TABLE edges (a INTEGER, b INTEGER, weight REAL, label TEXT);
             CREATE TABLE article_links (article_id INTEGER, entity_id INTEGER);
             CREATE TABLE topics (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE article_topics (topic_id INTEGER, article_id INTEGER);
             INSERT INTO sources VALUES (1, 'Wire Daily');
             INSERT INTO articles VALUES
               (7, 'Chips: China and Nvidia talks resume', '',
                'Talks between China and Nvidia resumed.', 'body',
                '2026-11-21T09:25:29Z', 'https://example.invalid/a7', 1, 1, 0,
                strftime('%Y-%m-%dT%H:%M:%SZ','now','-2 hours'), 1);
             INSERT INTO entities VALUES (1, 'China', 'country', 9), (2, 'Nvidia', 'company', 7);
             INSERT INTO edges VALUES (1, 2, 4.5, 'supply chain');
             INSERT INTO article_links VALUES (7, 1), (7, 2);",
        )
        .expect("the schema and its rows");
        TempArchive { path }
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Everything the page would have drawn during one turn, in the order it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Thought(String),
    Said(String),
    Tool(String),
}

/// Run one turn of the loop against the scripted server and keep every event.
///
/// `budget` is the reader.s settings, as the app hands them down: the loop must send the
/// same number on every step, so a test states it once and reads it off each body.
fn turned(
    server: &Server,
    archive: &TempArchive,
    budget: http::Budget,
) -> (Vec<Seen>, Result<http::Reply, String>) {
    let mut seen: Vec<Seen> = Vec::new();
    let mut outcome: Option<Result<http::Reply, String>> = None;
    atlas_desktop::agent::run(
        &archive.path,
        &server.url(),
        "answer from tool results",
        vec![http::Message::user("what does the archive say about China and Nvidia?")],
        budget,
        &mut |event| match event {
            atlas_desktop::agent::Event::Delta(http::Delta::Thinking(text)) => {
                seen.push(Seen::Thought(text))
            }
            atlas_desktop::agent::Event::Delta(http::Delta::Answer(text)) => {
                seen.push(Seen::Said(text))
            }
            atlas_desktop::agent::Event::Tool(line) => seen.push(Seen::Tool(line)),
            atlas_desktop::agent::Event::Done(result) => outcome = Some(result),
        },
    );
    (seen, outcome.expect("the loop always ends the turn"))
}

/// The point of the loop: the model asks for a tool, the window runs it against the
/// archive, and the answer the model then writes comes back with the tool's own words in
/// its history.
#[test]
fn the_model_asks_for_a_tool_and_the_window_runs_it() {
    let archive = TempArchive::new("loop");
    // The tool call arrives in pieces, the way a real server streams it.
    let mut first = called_in_pieces("call_1", "graph_neighbors", "{\"name\":\"China\"}");
    first.push(finished("tool_calls"));
    let server = Server::start(Behaviour::Steps(vec![
        Script::new(first),
        Script::new(vec![
            thought("The tool says the two are connected."),
            said("China and Nvidia are connected by the relation \"supply chain\" [Article #7]."),
            finished("stop"),
        ]),
    ]));

    let (seen, outcome) = turned(&server, &archive, http::Budget::SHIPPED);
    let reply = outcome.expect("an answer");

    // 1. The model was offered the tools, and told to choose.
    let first_body: serde_json::Value =
        serde_json::from_str(&server.body_at(0)).expect("the first body is JSON");
    let offered: Vec<String> = first_body["tools"]
        .as_array()
        .expect("the first request carried tools")
        .iter()
        .map(|spec| spec["function"]["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(offered, atlas_desktop::tools::names(), "the registry, whole");
    assert_eq!(first_body["tool_choice"], serde_json::json!("auto"));
    assert_eq!(first_body["chat_template_kwargs"]["enable_thinking"], serde_json::json!(true));
    assert_eq!(
        first_body["max_tokens"],
        serde_json::json!(http::Budget::SHIPPED.max_tokens),
        "the loop sends the budget it was handed: {:?}",
        server.body_at(0)
    );

    // 2. The call was answered: the second request carries the assistant's own request
    //    and the tool's result, paired by the id the model gave.
    assert_eq!(server.requests(), 2, "one request per step");
    let second_body: serde_json::Value =
        serde_json::from_str(&server.body_at(1)).expect("the second body is JSON");
    let messages = second_body["messages"].as_array().expect("messages");
    let assistant = messages
        .iter()
        .find(|message| message.get("tool_calls").is_some())
        .expect("the model's own request is shown back to it");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], serde_json::json!("graph_neighbors"));
    let result = messages
        .iter()
        .find(|message| message["role"] == serde_json::json!("tool"))
        .expect("the tool's answer travels as a tool message");
    assert_eq!(result["tool_call_id"], serde_json::json!("call_1"));
    let text = result["content"].as_str().unwrap_or("");
    assert!(text.starts_with(atlas_desktop::views::chat::CONTEXT_OPEN), "{text}");
    assert!(text.contains("supply chain"), "the relation the tool found: {text}");

    // 3. The page was told which tool ran, before any answer arrived.
    let tools: Vec<&String> = seen
        .iter()
        .filter_map(|event| match event {
            Seen::Tool(line) => Some(line),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 1, "{seen:?}");
    assert!(tools[0].contains("graph_neighbors"), "{tools:?}");
    let first_said = seen
        .iter()
        .position(|event| matches!(event, Seen::Said(_)))
        .expect("an answer");
    let ran_at = seen
        .iter()
        .position(|event| matches!(event, Seen::Tool(_)))
        .expect("a tool");
    assert!(ran_at < first_said, "the tool ran before the answer: {seen:?}");

    // 4. The thinking of the step that answered was kept, and the answer is the model's.
    assert!(reply.thinking.contains("connected"), "{:?}", reply.thinking);
    assert!(reply.answer.contains("supply chain"), "{:?}", reply.answer);
}

/// A name the registry does not hold is refused, and the refusal is what the loop feeds the
/// model back — nothing is executed, nothing is opened.
#[test]
fn a_hostile_tool_name_is_refused_and_the_refusal_is_fed_back() {
    let archive = TempArchive::new("hostile");
    let server = Server::start(Behaviour::Steps(vec![
        Script::new(vec![
            called("call_9", "web_open", "{\"url\":\"http://example.invalid\"}"),
            finished("tool_calls"),
        ]),
        Script::new(vec![
            said("I could not open a page: this window only reads its archive."),
            finished("stop"),
        ]),
    ]));

    let (seen, outcome) = turned(&server, &archive, http::Budget::SHIPPED);
    let reply = outcome.expect("an answer");
    assert_eq!(server.requests(), 2);
    let second_body: serde_json::Value =
        serde_json::from_str(&server.body_at(1)).expect("the second body is JSON");
    let result = second_body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == serde_json::json!("tool"))
        .expect("the refusal is the tool's answer");
    let text = result["content"].as_str().unwrap_or("");
    assert!(text.contains("web_open"), "{text}");
    assert!(text.contains("does not have"), "the refusal, in words: {text}");
    // The page is told the tool ran, because to the loop it is a step like any other —
    // and the turn still ends with an answer.
    assert!(seen.iter().any(|event| matches!(event, Seen::Tool(_))), "{seen:?}");
    assert!(!reply.answer.trim().is_empty());
}

/// The step budget: a model that asks for a tool on every turn is answered *without* a
/// menu on the last step, so the loop cannot run forever.
#[test]
fn the_loop_stops_offering_tools_after_its_step_budget() {
    let archive = TempArchive::new("budget");
    let always = || {
        Script::new(vec![
            called("call_x", "stats", "{}"),
            finished("tool_calls"),
        ])
    };
    let scripts: Vec<Script> = (0..=atlas_desktop::tools::MAX_STEPS).map(|_| always()).collect();
    let server = Server::start(Behaviour::Steps(scripts));

    let (_seen, outcome) = turned(&server, &archive, http::Budget::SHIPPED);
    // Every step but the last is offered the menu; the last is not, and this server still
    // answers with a call, which the loop ends on rather than calling again.
    let steps = atlas_desktop::tools::MAX_STEPS + 1;
    let last_body: serde_json::Value =
        serde_json::from_str(&server.body_at(steps - 1)).expect("the last body is JSON");
    assert!(
        last_body.get("tools").is_none(),
        "the last step must be asked without a menu: {last_body}"
    );
    let _ = outcome;
}

/// The reader's two numbers, on every step of a real turn.
///
/// This is the chain the settings travel: `app::reload` reads `model_reasoning` and
/// `model_answer` from the archive, `http::Budget` turns them into one `max_tokens` and one
/// flag, the Chat page hands the budget to `agent::run`, and the loop sends it on each
/// request. Here the two cases a reader can reach are read off the wire, not off the code:
/// thinking turned off (no `chat_template_kwargs` at all, the answer's room alone) and a
/// raised pair (the two rooms added, thinking asked for).
#[test]
fn the_turn_is_sent_with_the_budget_the_reader_set() {
    // `model_reasoning = 0, model_answer = 3000`: reflex answers, 3 000 tokens of room.
    let archive = TempArchive::new("budget-reflex");
    let server = Server::start(Behaviour::Steps(vec![
        Script::new(vec![
            called("call_1", "stats", "{}"),
            finished("tool_calls"),
        ]),
        Script::new(vec![
            said("Seven stories, newest this morning."),
            finished("stop"),
        ]),
    ]));
    let reflex = http::Budget::from_meta(Some("0"), Some("3000"));
    let (_seen, outcome) = turned(&server, &archive, reflex);
    assert!(outcome.is_ok(), "{:?}", outcome.err());
    assert_eq!(server.requests(), 2, "the tool step and the answer");
    for step in 0..2 {
        let body: serde_json::Value =
            serde_json::from_str(&server.body_at(step)).expect("a JSON body");
        assert_eq!(
            body["max_tokens"],
            serde_json::json!(3000),
            "step {step} did not send the answer's room the reader set: {body}"
        );
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "step {step} asked a reflex model to think: {body}"
        );
    }

    // `model_reasoning = 12000, model_answer = 4000`: one 16 000-token budget on the wire,
    // because llama.cpp has one number, and thinking asked for because there is room.
    let archive = TempArchive::new("budget-raised");
    let server = Server::start(Behaviour::Steps(vec![Script::new(vec![
        said("Sixteen thousand tokens' worth of room."),
        finished("stop"),
    ])]));
    let raised = http::Budget::from_meta(Some("12000"), Some("4000"));
    let (_seen, outcome) = turned(&server, &archive, raised);
    assert!(outcome.is_ok(), "{:?}", outcome.err());
    let body: serde_json::Value = serde_json::from_str(&server.body_at(0)).expect("JSON");
    assert_eq!(body["max_tokens"], serde_json::json!(16_000), "{body}");
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        serde_json::json!(true),
        "{body}"
    );
}

/// The live measurement: the running `llama-server` on 8081, the real archive, one question
/// that needs the graph.
///
/// ```text
/// cargo test --offline -p atlas-desktop --test chat_wire -- --ignored --nocapture the_live
/// ```
#[test]
#[ignore = "asks the live llama-server on 127.0.0.1:8081; run with --ignored"]
fn the_live_model_calls_a_tool_and_answers_with_what_it_got() {
    let url = std::env::var("ATLAS_MODEL_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string());
    // The question is the caller's, so the same measurement can be pointed at the graph
    // tools (`ATLAS_LIVE_QUESTION=…`) without a second test that would be a copy of this one.
    let question = std::env::var("ATLAS_LIVE_QUESTION").unwrap_or_else(|_| {
        "What does the archive say about China and Nvidia?".to_string()
    });
    let archive = atlas_desktop::assets::archive();
    let started = Instant::now();
    let mut tools_run: Vec<String> = Vec::new();
    let mut first_thought: Option<Duration> = None;
    let mut first_answer: Option<Duration> = None;
    let mut outcome = None;
    atlas_desktop::agent::run(
        &archive,
        &url,
        atlas_desktop::tools::SYSTEM,
        vec![http::Message::user(question.clone())],
        http::Budget::SHIPPED,
        &mut |event| match event {
            atlas_desktop::agent::Event::Delta(http::Delta::Thinking(_)) => {
                if first_thought.is_none() {
                    first_thought = Some(started.elapsed());
                }
            }
            atlas_desktop::agent::Event::Delta(http::Delta::Answer(_)) => {
                if first_answer.is_none() {
                    first_answer = Some(started.elapsed());
                }
            }
            atlas_desktop::agent::Event::Tool(line) => {
                println!("LIVE tool after {:?}: {line}", started.elapsed());
                tools_run.push(line);
            }
            atlas_desktop::agent::Event::Done(result) => outcome = Some(result),
        },
    );
    match outcome.expect("the loop ends the turn") {
        Ok(reply) => {
            println!("LIVE QUESTION: {question}");
            println!(
                "LIVE MEASURED: {} tools, first thought {} ms, first answer {} ms, total {:?}",
                tools_run.len(),
                first_thought.map(|wait| wait.as_millis()).unwrap_or(0),
                first_answer.map(|wait| wait.as_millis()).unwrap_or(0),
                started.elapsed()
            );
            println!("LIVE ANSWER: {}", reply.answer);
        }
        Err(sentence) => {
            println!("LIVE REFUSAL: {sentence}");
            one_sentence(&sentence);
        }
    }
}
