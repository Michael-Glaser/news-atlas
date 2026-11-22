//! HTTP/1.1 on a loopback socket: the request, the response, the framing.
//!
//! Decisions: `Connection: close` on every exchange (no pool to leak), the connect
//! timeout and the total deadline, the framing (`chunked`, `Content-Length`, or
//! read-to-EOF), the body cap, and that a status line which is not `HTTP/` is a
//! [`AiError::BadResponse`]. The request is written under the same deadline it is
//! read under.
//!
//! It does not: choose the endpoint ([`crate::endpoint`]), decode chunked framing
//! ([`crate::chunked`]), keep the clock ([`crate::deadline`]) or retry
//! ([`crate::retry`]). It never speaks TLS and never leaves the loopback address.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::chunked::ChunkedDecoder;
use crate::deadline::{timed_out, Deadline};
use crate::endpoint::Endpoint;
use crate::error::AiError;
use crate::retry::retry_once;

/// Cap on one response header block. A model server sends a handful of short
/// headers; anything larger is not one.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Default cap on one response body: a 6144-token context cannot produce more
/// than a few hundred kilobytes of JSON, and a stream should sit far below this.
pub const DEFAULT_MAX_BODY: usize = 8 * 1024 * 1024;

/// Default time to a connected socket.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many times a *transient* failure is retried. One, per the contract: a
/// llama.cpp still loading its model refuses a connection for a moment, and a
/// second attempt either works or is a real failure.
pub const DEFAULT_RETRIES: u8 = 1;

/// The most retries a caller may ask for.
///
/// `with_retries` takes a `u8`, and a caller that passes `255` would be asking one
/// request to hammer a loopback port 256 times — with a model on the other end, each
/// attempt can cost a whole generation. Three attempts is already more than the
/// contract needs; anything larger is clamped, and the clamp is documented here
/// rather than being a silent surprise.
pub const MAX_RETRIES: u8 = 3;

/// The longest deadline one exchange may be given.
///
/// An hour is longer than any local generation: a small model writing a briefing on
/// this machine takes seconds, and a call that has not started answering by then is
/// not going to. A caller passing `u64::MAX` seconds would otherwise pin a request
/// thread for as long as the process lives.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// The shortest deadline: one millisecond. Zero would fail before the connect.
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1);

fn clamp_timeout(timeout: Duration) -> Duration {
    timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT)
}

// ---------------------------------------------------------------------------
// the client
// ---------------------------------------------------------------------------

/// One loopback HTTP/1.1 client. Cheap to build and holds no connection between
/// calls (`Connection: close`).
#[derive(Debug, Clone)]
pub struct LoopbackHttp {
    endpoint: Endpoint,
    timeout: Duration,
    connect_timeout: Duration,
    max_body: usize,
    retries: u8,
}

impl LoopbackHttp {
    /// A client for `endpoint` with a total deadline of `timeout` per exchange.
    ///
    /// The deadline is bounded here, once: a zero duration would make every
    /// exchange fail before it started, and anything past [`MAX_TIMEOUT`] is not a
    /// call that will ever finish.
    pub fn new(endpoint: Endpoint, timeout: Duration) -> LoopbackHttp {
        LoopbackHttp {
            endpoint,
            timeout: clamp_timeout(timeout),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_body: DEFAULT_MAX_BODY,
            retries: DEFAULT_RETRIES,
        }
    }

    pub fn with_max_body(mut self, max_body: usize) -> LoopbackHttp {
        self.max_body = max_body.max(1024);
        self
    }

    /// How many transient failures to retry, clamped to [`MAX_RETRIES`].
    pub fn with_retries(mut self, retries: u8) -> LoopbackHttp {
        self.retries = retries.min(MAX_RETRIES);
        self
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> LoopbackHttp {
        self.connect_timeout = clamp_timeout(timeout);
        self
    }

    /// The same client with a different total deadline for one exchange.
    ///
    /// It does not rebuild the client: the body cap, the retry count and the
    /// connect timeout a caller already chose have to survive a later
    /// `.timeout(...)` in a builder chain.
    pub fn with_timeout(mut self, timeout: Duration) -> LoopbackHttp {
        self.timeout = clamp_timeout(timeout);
        self
    }

    /// The deadline one exchange gets.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The cap on one response body.
    pub fn max_body(&self) -> usize {
        self.max_body
    }

    /// How many transient failures are retried (at most [`MAX_RETRIES`]).
    pub fn retries(&self) -> u8 {
        self.retries
    }

    /// The budget for opening the socket.
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// POST a JSON body and hand back the (streaming) response.
    pub fn post_json(&self, path: &str, body: &str) -> Result<Response, AiError> {
        retry_once(self.retries, || {
            self.exchange(
                "POST",
                path,
                Some(body),
                "text/event-stream, application/json",
            )
        })
    }

    /// GET a small JSON document and read it whole: `(status, body)`.
    pub fn get_json(&self, path: &str) -> Result<(u16, String), AiError> {
        let mut response =
            retry_once(self.retries, || self.exchange("GET", path, None, "application/json"))?;
        let status = response.status();
        Ok((status, response.read_text()?))
    }

    fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        accept: &str,
    ) -> Result<Response, AiError> {
        let deadline = Deadline::new(self.timeout);
        let stream = self.connect(&deadline)?;
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nAccept: {}\r\nConnection: close\r\nUser-Agent: atlas-ai/0.1\r\n",
            method,
            self.endpoint.path(path),
            self.endpoint.authority,
            accept
        );
        let payload = body.unwrap_or("");
        if body.is_some() {
            head.push_str("Content-Type: application/json\r\n");
            head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        head.push_str("\r\n");

        {
            let mut sink = TimedWrite {
                stream: &stream,
                deadline: &deadline,
            };
            sink.write_all(head.as_bytes())?;
            if body.is_some() {
                sink.write_all(payload.as_bytes())?;
            }
            sink.flush()?;
        }
        read_head(stream, deadline, self.max_body)
    }

    fn connect(&self, deadline: &Deadline) -> Result<TcpStream, AiError> {
        let addr = self.endpoint.addr();
        let remaining = deadline
            .remaining()
            .ok_or_else(|| AiError::Timeout("no time left to connect".into()))?;
        let budget = remaining
            .min(self.connect_timeout)
            .max(Duration::from_millis(1));
        let stream = TcpStream::connect_timeout(&addr, budget).map_err(|e| {
            AiError::Unreachable(format!(
                "cannot connect to the loopback model server ({})",
                e.kind()
            ))
        })?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(budget));
        let _ = stream.set_write_timeout(Some(budget));
        Ok(stream)
    }
}

/// A write with the request's deadline applied.
struct TimedWrite<'a> {
    stream: &'a TcpStream,
    deadline: &'a Deadline,
}

impl TimedWrite<'_> {
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), AiError> {
        while !bytes.is_empty() {
            let remaining = self
                .deadline
                .remaining()
                .ok_or_else(|| AiError::Timeout("the request took too long to send".into()))?;
            let _ = self
                .stream
                .set_write_timeout(Some(remaining.max(Duration::from_millis(1))));
            match (&*self.stream).write(bytes) {
                Ok(0) => {
                    return Err(AiError::Unreachable(
                        "the loopback model server closed while reading the request".into(),
                    ))
                }
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if timed_out(&e) => {
                    return Err(AiError::Timeout("the request took too long to send".into()))
                }
                Err(e) => {
                    return Err(AiError::Unreachable(format!(
                        "cannot send to the loopback model server ({})",
                        e.kind()
                    )))
                }
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), AiError> {
        (&*self.stream)
            .flush()
            .map_err(|e| AiError::Unreachable(format!("cannot flush ({})", e.kind())))
    }
}

// ---------------------------------------------------------------------------
// response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Framing {
    Chunked,
    Length(usize),
    UntilEof,
}

/// One response: the status, the headers that matter, and a byte reader that
/// enforces the deadline, the framing and the cap.
pub struct Response {
    status: u16,
    content_type: String,
    framing: Framing,
    stream: TcpStream,
    deadline: Deadline,
    /// Decoded bytes that have not been handed out yet.
    pending: Vec<u8>,
    /// Bytes still expected for `Framing::Length`.
    left: usize,
    decoder: ChunkedDecoder,
    read_total: usize,
    max_body: usize,
    eof: bool,
}

impl Response {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Read whatever is available into `buf`; `Ok(0)` means the body ended.
    pub fn read_some(&mut self, buf: &mut [u8]) -> Result<usize, AiError> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.pending.is_empty() {
                let take = self.pending.len().min(buf.len());
                buf[..take].copy_from_slice(&self.pending[..take]);
                self.pending.drain(..take);
                return Ok(take);
            }
            if self.eof {
                return Ok(0);
            }
            self.pump()?;
            if self.pending.is_empty() && self.eof {
                return Ok(0);
            }
        }
    }

    /// Read the whole body as text (lossy UTF-8: bad bytes are not a failure).
    /// For the small JSON calls only — a caller that streams uses `read_some`.
    pub fn read_text(&mut self) -> Result<String, AiError> {
        let mut out: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = self.read_some(&mut buf)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    /// One socket read, decoded into `pending`.
    fn pump(&mut self) -> Result<(), AiError> {
        let mut scratch = [0u8; 16 * 1024];
        match self.framing {
            Framing::Length(_) => {
                if self.left == 0 {
                    self.eof = true;
                    return Ok(());
                }
                let want = self.left.min(scratch.len());
                let read = self.socket_read(&mut scratch[..want])?;
                if read == 0 {
                    return Err(AiError::Protocol(format!(
                        "the body ended {} bytes early",
                        self.left
                    )));
                }
                self.left -= read;
                self.pending.extend_from_slice(&scratch[..read]);
                self.read_total += read;
            }
            Framing::UntilEof => {
                let read = self.socket_read(&mut scratch)?;
                if read == 0 {
                    self.eof = true;
                    return Ok(());
                }
                self.pending.extend_from_slice(&scratch[..read]);
                self.read_total += read;
            }
            Framing::Chunked => {
                if self.decoder.finished() {
                    self.eof = true;
                    return Ok(());
                }
                let read = self.socket_read(&mut scratch)?;
                if read == 0 {
                    return Err(AiError::Protocol(
                        "the chunked body ended before its final chunk".into(),
                    ));
                }
                let mut decoded: Vec<u8> = Vec::new();
                let done = self.decoder.push(&scratch[..read], &mut decoded)?;
                if done {
                    self.eof = true;
                }
                self.read_total += decoded.len();
                self.pending.extend_from_slice(&decoded);
            }
        }
        if self.read_total > self.max_body {
            return Err(AiError::Protocol(format!(
                "the response is over the {} byte cap",
                self.max_body
            )));
        }
        Ok(())
    }

    fn socket_read(&mut self, buf: &mut [u8]) -> Result<usize, AiError> {
        loop {
            let remaining = self
                .deadline
                .remaining()
                .ok_or_else(|| AiError::Timeout("the model did not answer in time".into()))?;
            let _ = self
                .stream
                .set_read_timeout(Some(remaining.max(Duration::from_millis(1))));
            match (&self.stream).read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if timed_out(&e) => {
                    return Err(AiError::Timeout("the model did not answer in time".into()))
                }
                Err(e) => {
                    return Err(AiError::Unreachable(format!(
                        "the loopback model server dropped the connection ({})",
                        e.kind()
                    )))
                }
            }
        }
    }
}

/// Read the status line and headers, then hand back a [`Response`] that knows how
/// to frame the body.
fn read_head(stream: TcpStream, deadline: Deadline, max_body: usize) -> Result<Response, AiError> {
    let mut head: Vec<u8> = Vec::with_capacity(2048);
    let mut scratch = [0u8; 4096];
    loop {
        let remaining = deadline
            .remaining()
            .ok_or_else(|| AiError::Timeout("the model did not answer in time".into()))?;
        let _ = stream.set_read_timeout(Some(remaining.max(Duration::from_millis(1))));
        let n = match (&stream).read(&mut scratch) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if timed_out(&e) => {
                return Err(AiError::Timeout("the model did not answer in time".into()))
            }
            Err(e) => {
                return Err(AiError::Unreachable(format!(
                    "the loopback model server dropped the connection ({})",
                    e.kind()
                )))
            }
        };
        if n == 0 {
            return Err(AiError::BadResponse(
                "the model server closed before sending a status line".into(),
            ));
        }
        head.extend_from_slice(&scratch[..n]);
        if head.len() > MAX_HEADER_BYTES {
            return Err(AiError::Protocol(format!(
                "the response headers are over the {} byte cap",
                MAX_HEADER_BYTES
            )));
        }
        if let Some((end, consumed)) = header_end(&head) {
            let block = head[..end].to_vec();
            let mut rest = head[consumed..].to_vec();
            let (status, headers) = parse_head(&block)?;
            let mut response = Response {
                status,
                content_type: headers.content_type,
                framing: headers.framing.clone(),
                stream,
                deadline,
                pending: Vec::new(),
                left: 0,
                decoder: ChunkedDecoder::new(),
                read_total: 0,
                max_body,
                eof: false,
            };
            match headers.framing {
                Framing::Length(n) => {
                    // The header remainder is body bytes, capped at the length.
                    rest.truncate(n);
                    response.left = n - rest.len();
                    if n == 0 {
                        response.eof = true;
                    } else if response.left == 0 {
                        response.eof = true;
                    }
                    response.pending = rest;
                }
                Framing::UntilEof => {
                    response.pending = rest;
                }
                Framing::Chunked => {
                    let mut decoded: Vec<u8> = Vec::new();
                    let done = response.decoder.push(&rest, &mut decoded)?;
                    if done {
                        response.eof = true;
                    }
                    response.pending = decoded;
                }
            }
            return Ok(response);
        }
    }
}

/// Where the header block ends: `(offset of the terminator, bytes consumed)`.
fn header_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\r'
            && i + 3 < bytes.len()
            && bytes[i + 1] == b'\n'
            && bytes[i + 2] == b'\r'
            && bytes[i + 3] == b'\n'
        {
            return Some((i, i + 4));
        }
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            return Some((i, i + 2));
        }
        i += 1;
    }
    None
}

struct Headers {
    framing: Framing,
    content_type: String,
}

fn parse_head(block: &[u8]) -> Result<(u16, Headers), AiError> {
    let text = String::from_utf8_lossy(block);
    let mut lines = text.split('\n');
    let status_line = lines.next().unwrap_or("").trim_end_matches('\r');
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(AiError::BadResponse(
            "the answer is not an HTTP response".into(),
        ));
    }
    let status: u16 = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| AiError::BadResponse("the status line has no status code".into()))?;

    let mut chunked = false;
    let mut length: Option<usize> = None;
    let mut content_type = String::new();
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (name, value) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "transfer-encoding" => {
                if value.to_ascii_lowercase().contains("chunked") {
                    chunked = true;
                }
            }
            "content-length" => length = value.trim().parse::<usize>().ok(),
            "content-type" => content_type = value.trim().to_string(),
            _ => {}
        }
    }
    let framing = if chunked {
        Framing::Chunked
    } else {
        match length {
            Some(n) => Framing::Length(n),
            None => Framing::UntilEof,
        }
    };
    Ok((
        status,
        Headers {
            framing,
            content_type,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn header_end_finds_both_line_endings() {
        assert_eq!(header_end(b"a\r\n\r\nrest"), Some((1, 5)));
        assert_eq!(header_end(b"a\n\nrest"), Some((1, 3)));
        assert_eq!(header_end(b"a\r\nb: 1"), None);
    }

    #[test]
    fn head_parsing_reads_status_framing_and_type() {
        let (status, headers) =
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked")
                .unwrap();
        assert_eq!(status, 200);
        assert_eq!(headers.framing, Framing::Chunked);
        assert_eq!(headers.content_type, "text/event-stream");

        let (status, headers) = parse_head(b"HTTP/1.1 404 Not Found\r\nContent-Length: 12").unwrap();
        assert_eq!(status, 404);
        assert_eq!(headers.framing, Framing::Length(12));

        let (_, headers) = parse_head(b"HTTP/1.1 200 OK").unwrap();
        assert_eq!(headers.framing, Framing::UntilEof);
    }

    #[test]
    fn a_client_is_bounded_whatever_it_is_given() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8080").unwrap();
        // A zero deadline would fail before the connect; a year is not a call.
        assert_eq!(
            LoopbackHttp::new(endpoint.clone(), Duration::ZERO).timeout(),
            MIN_TIMEOUT
        );
        assert_eq!(
            LoopbackHttp::new(endpoint.clone(), Duration::from_secs(31_536_000)).timeout(),
            MAX_TIMEOUT
        );
        assert_eq!(
            LoopbackHttp::new(endpoint, Duration::from_secs(600)).timeout(),
            Duration::from_secs(600),
            "a sane deadline is untouched"
        );
        // Retries are clamped, and the defaults are the documented ones.
        let client = LoopbackHttp::new(
            Endpoint::parse("http://127.0.0.1:8080").unwrap(),
            Duration::from_secs(1),
        );
        assert_eq!(client.retries(), DEFAULT_RETRIES);
        assert_eq!(client.max_body(), DEFAULT_MAX_BODY);
        assert_eq!(client.clone().with_retries(255).retries(), MAX_RETRIES);
        assert_eq!(
            client.with_retries(0).retries(),
            0,
            "zero retries is a choice, not a mistake to correct"
        );
    }
}
