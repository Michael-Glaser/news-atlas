//! A loopback HTTP server for this crate's tests, and nothing else.
//!
//! The transport's promises — the body cap, the hop bound, the refusal of a hop
//! *before* it is requested, the read timeout, the whole-fetch budget — can only be
//! proven against a server that misbehaves on purpose. This one listens on
//! `127.0.0.1:0` inside the test process (the kernel picks a free port, so nothing
//! outside this process is ever listening), answers from a script, and records every
//! request it received so a test can assert what was *not* sent.
//!
//! It decides nothing about the app: it is a test double, compiled only under
//! `cfg(test)`, and it never leaves this crate.
//!
//! Nothing here is a plain `TcpListener` loop with no ending: [`Server`] polls a
//! stop flag, so dropping it always ends the thread and the test never leaks one.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One request exactly as it arrived, so a test can assert on what was sent.
#[derive(Debug, Clone, Default)]
pub struct Received {
    /// The request line, e.g. `GET /a?b=1 HTTP/1.1`.
    pub line: String,
    /// Header name/value pairs, names lower-cased.
    pub headers: Vec<(String, String)>,
}

impl Received {
    /// One header's value, or `""` when the client did not send it.
    pub fn header(&self, name: &str) -> &str {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    }

    /// The request target, e.g. `/a?b=1`.
    pub fn path(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or("")
    }

    /// The method, e.g. `GET`.
    pub fn method(&self) -> &str {
        self.line.split(' ').next().unwrap_or("")
    }
}

/// What the server answers for one request.
#[derive(Debug, Clone)]
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
    /// Answer nothing at all and hold the connection open: the shape of a site
    /// that accepts the connection and then goes quiet.
    silent: bool,
    /// Leave the connection open for another request, as a real HTTP/1.1 server
    /// does, so a test can see whether the client reuses it.
    keep_alive: bool,
    /// Write the whole response one byte at a time, this long apart: the shape of a
    /// slow-loris site, which answers immediately and then dribbles.
    dribble: Duration,
    /// Dribble the status line and headers too, not only the body. Two shapes, two
    /// budgets to spend: a site that is slow to answer and a site that is slow to
    /// finish always look the same from the client's side.
    dribble_head: bool,
}

impl Reply {
    /// A 200 with an HTML content type.
    pub fn html(body: &str) -> Reply {
        Reply::typed(200, "text/html; charset=utf-8", body.as_bytes().to_vec())
    }

    /// A response with its own status, content type and body.
    pub fn typed(status: u16, content_type: &str, body: Vec<u8>) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type".to_string(), content_type.to_string())],
            body,
            delay: Duration::ZERO,
            silent: false,
            keep_alive: false,
            dribble: Duration::ZERO,
            dribble_head: false,
        }
    }

    /// A redirect to `location`, with the status a real site uses.
    pub fn redirect(location: &str) -> Reply {
        Reply::redirect_with(302, location)
    }

    /// A redirect with an explicit status (301, 303, 307, 308).
    pub fn redirect_with(status: u16, location: &str) -> Reply {
        let mut reply = Reply::typed(status, "text/html", Vec::new());
        reply.headers.push(("Location".to_string(), location.to_string()));
        reply
    }

    /// Wait this long before answering: the shape of a slow site.
    pub fn after(mut self, delay: Duration) -> Reply {
        self.delay = delay;
        self
    }

    /// Accept the connection and never answer.
    pub fn silent() -> Reply {
        Reply {
            status: 0,
            headers: Vec::new(),
            body: Vec::new(),
            delay: Duration::ZERO,
            silent: true,
            keep_alive: false,
            dribble: Duration::ZERO,
            dribble_head: false,
        }
    }

    /// Answer at once, but one byte at a time with `per_byte` between writes: the
    /// shape of a slow-loris site. The head dribbles too.
    pub fn dribbled(per_byte: Duration) -> Reply {
        Reply::html("<html><body>dribbled</body></html>")
            .dribbling(per_byte)
            .dribbling_head()
    }

    /// The same, on a reply that already has a body and a status: the head arrives at
    /// once and the body dribbles, which is the shape that spends the read budget.
    pub fn dribbling(mut self, per_byte: Duration) -> Reply {
        self.dribble = per_byte;
        self
    }

    /// Dribble the status line and headers as well as the body.
    pub fn dribbling_head(mut self) -> Reply {
        self.dribble_head = true;
        self
    }

    /// One more header on the answer.
    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Leave the connection open after answering, as a real HTTP/1.1 server does.
    /// A test that sends two requests then can see whether the client reused it.
    pub fn keep_alive(mut self) -> Reply {
        self.keep_alive = true;
        self
    }
}

/// The script the server follows: request number (from 0) plus what arrived.
type Script = dyn Fn(usize, &Received) -> Reply + Send + Sync;

/// A loopback HTTP server, running on its own thread until it is dropped.
pub struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Received>>>,
    connections: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Start a server that always answers the same thing.
    pub fn always(reply: Reply) -> Server {
        Server::scripted(move |_, _| reply.clone())
    }

    /// Start a server whose answer depends on the request number and content.
    pub fn scripted(script: impl Fn(usize, &Received) -> Reply + Send + Sync + 'static) -> Server {
        // `127.0.0.1:0`: the kernel picks the port, so two tests can run at once
        // and nothing outside this process is ever listening.
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback test port");
        let port = listener.local_addr().expect("the test port").port();
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let script: Arc<Script> = Arc::new(script);
        let thread = {
            let stop = Arc::clone(&stop);
            let seen = Arc::clone(&seen);
            let connections = Arc::clone(&connections);
            std::thread::spawn(move || accept_loop(listener, stop, seen, connections, script))
        };
        Server {
            port,
            stop,
            seen,
            connections,
            thread: Some(thread),
        }
    }

    /// The address this server listens on (`127.0.0.1:<port>`).
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// A URL on this server: `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// The guard's token for one URL on this server.
    ///
    /// A test needs the app's own door to exercise the transport at all: the public
    /// gate refuses every loopback address, on purpose, and a test that could reach a
    /// socket without that door would be testing a different client than the one
    /// that ships.
    pub fn token(&self, path: &str) -> atlas_guard::Gated {
        atlas_guard::Gated::loopback(&self.url(path)).expect("a loopback verdict for the test server")
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Received> {
        self.seen.lock().expect("the request log").clone()
    }

    /// How many requests have been received.
    pub fn count(&self) -> usize {
        self.seen.lock().expect("the request log").len()
    }

    /// How many TCP connections have been accepted. Two requests over one
    /// connection is what connection reuse looks like from the server's side.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // The accept loop polls the flag every few milliseconds, so this join is
            // bounded even when the last connection was a silent one.
            let _ = thread.join();
        }
    }
}

fn accept_loop(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Received>>>,
    connections: Arc<AtomicUsize>,
    script: Arc<Script>,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                connections.fetch_add(1, Ordering::SeqCst);
                serve(stream, &seen, &script, &stop);
            }
            // Nothing waiting: yield the CPU rather than spinning hot, and keep the
            // delay far below the cost of a connect so a measurement of the
            // transport is not a measurement of this loop.
            Err(_) => std::thread::sleep(Duration::from_micros(50)),
        }
    }
}

/// Answer the requests on one connection, one at a time, until the connection
/// closes (or the reply says to close it).
fn serve(
    mut stream: TcpStream,
    seen: &Arc<Mutex<Vec<Received>>>,
    script: &Arc<Script>,
    stop: &Arc<AtomicBool>,
) {
    // On BSD and macOS an accepted socket inherits the listener's non-blocking
    // mode, which would make the first read return `WouldBlock` before the client
    // has written anything. The connection is blocking; the listener is not.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    loop {
        let head = read_head(&mut stream, stop);
        if head.is_empty() {
            return;
        }
        let received = parse_head(&head);
        let index = {
            let mut log = seen.lock().expect("the request log");
            log.push(received.clone());
            log.len() - 1
        };
        let reply = script(index, &received);

        if reply.silent {
            // Hold the connection, answer nothing, and stay interruptible so a drop
            // cannot hang the test binary.
            let until = Instant::now() + Duration::from_secs(5);
            while !stop.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            return;
        }
        if !reply.delay.is_zero() {
            let until = Instant::now() + reply.delay;
            while !stop.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if write_reply(&mut stream, &reply, stop).is_err() {
            return;
        }
        if !reply.keep_alive {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
    }
}

/// Write one response. A client that hit its cap hung up already: the write failing
/// is the expected outcome of that test, not an error worth reporting.
///
/// A dribbled reply writes the same bytes one at a time with the reply's interval
/// between them, and stays interruptible so dropping the `Server` cannot leave the
/// test binary waiting for a slow site that nobody is listening to any more.
fn write_reply(
    stream: &mut TcpStream,
    reply: &Reply,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let mut answer = format!(
        "HTTP/1.1 {} {}\r\n",
        reply.status,
        reason_phrase(reply.status)
    );
    for (name, value) in &reply.headers {
        answer.push_str(&format!("{}: {}\r\n", name, value));
    }
    answer.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    answer.push_str(if reply.keep_alive {
        "Connection: keep-alive\r\n\r\n"
    } else {
        "Connection: close\r\n\r\n"
    });
    let mut framed = answer.into_bytes();
    let head_len = framed.len();
    framed.extend_from_slice(&reply.body);
    if reply.dribble.is_zero() {
        stream.write_all(&framed)?;
        return stream.flush();
    }
    // The head whole when the reply only dribbles its body; otherwise every byte
    // waits, which is the shape of a site that is slow to answer at all.
    let from = if reply.dribble_head { 0 } else { head_len };
    stream.write_all(&framed[..from])?;
    stream.flush()?;
    for byte in &framed[from..] {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        stream.write_all(&[*byte])?;
        stream.flush()?;
        let until = Instant::now() + reply.dribble;
        while !stop.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

/// A valid `gzip` member that holds exactly `data`, built without a compressor.
///
/// The client asks for `identity`, so a `gzip` answer is a server ignoring the
/// request — and the bytes of a real gzip stream are what makes the difference
/// between "we refused to decompress it" and "we tried and it was not gzip". Built
/// here rather than embedded as a fixture because a fixture is a binary blob in a
/// source tree, and this is fifteen lines of a format RFC 1951 writes down.
pub fn gzip_of(data: &[u8]) -> Vec<u8> {
    // A `gzip` member whose DEFLATE blocks are *stored*, which is legal, expands to
    // exactly `data`, and needs no compressor: RFC 1951 §3.2.4, five bytes of block
    // header for up to 65 535 bytes of payload.
    let mut out: Vec<u8> = vec![
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
    ];
    let mut rest = data;
    while !rest.is_empty() {
        let take = rest.len().min(65_535);
        let last = take == rest.len();
        out.push(if last { 0x01 } else { 0x00 });
        out.extend_from_slice(&(take as u16).to_le_bytes());
        out.extend_from_slice(&(!(take as u16)).to_le_bytes());
        out.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// CRC-32 (the polynomial gzip's trailer carries), for a payload this server made.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Read the request head. A TLS ClientHello (an `https` URL pointed at this plain
/// server) carries no `\r\n\r\n`, so a non-printable byte ends the read at once
/// instead of waiting out the socket timeout.
fn read_head(stream: &mut TcpStream, stop: &AtomicBool) -> Vec<u8> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 512];
    let deadline = Instant::now() + Duration::from_secs(2);
    // A connection that is only waiting for a request it will never get must not
    // keep the test binary alive: the flag ends the wait as well as the deadline.
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                head.extend_from_slice(&chunk[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if chunk[..n]
                    .iter()
                    .any(|byte| *byte < 0x20 && *byte != b'\r' && *byte != b'\n')
                {
                    break;
                }
                if head.len() > 64 * 1024 {
                    break;
                }
            }
            // A read timeout with nothing after it: the client is quiet. Keep the
            // head that did arrive rather than losing a request with the answer.
            Err(error) => match error.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => {
                    if head.is_empty() {
                        continue;
                    }
                    break;
                }
                _ => break,
            },
        }
    }
    head
}

fn parse_head(head: &[u8]) -> Received {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let line = lines.next().unwrap_or("").trim().to_string();
    let mut headers = Vec::new();
    for raw in lines {
        let raw = raw.trim_end_matches('\r');
        if raw.is_empty() {
            break;
        }
        if let Some((name, value)) = raw.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Received { line, headers }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        404 => "Not Found",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
