//! Is the model server answering, and how long do we wait for it?
//!
//! `llama-server` answers `GET /health` on its own port: `200` once the weights
//! are loaded, `503` while it is still reading them, and nothing at all before the
//! socket exists. That is the one thing this module decides — it is a *process
//! readiness* rule, not a model-capability one, which is why it is a raw
//! loopback request here and `atlas-ai`'s `/v1/models` probe for the app.
//!
//! The request is HTTP/1.1 to a literal `127.0.0.1`, hand-written: no name
//! resolution, no client library, no TLS, no redirect. The response body is read
//! only to be discarded — it is the child's JSON, not ours to interpret — and
//! nothing from it is ever logged.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

/// How long one probe may take before it counts as "not answering yet".
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// The whole wait for the first answer, the way `run.sh` counted it (120 x 2 s).
pub const BOOT_DEADLINE: Duration = Duration::from_secs(240);

/// The gap between two probes.
pub const BOOT_INTERVAL: Duration = Duration::from_secs(2);

/// What `/health` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// `200`: the weights are loaded and the server is taking requests.
    Up,
    /// `503`: the process is alive and still loading.
    Loading,
    /// Nothing answered, with the reason.
    Down(String),
}

impl State {
    pub fn is_up(&self) -> bool {
        matches!(self, State::Up)
    }

    /// The words a person reads on the line.
    pub fn text(&self) -> String {
        match self {
            State::Up => "answering".to_string(),
            State::Loading => "still loading the model".to_string(),
            State::Down(reason) => format!("no answer ({})", reason),
        }
    }
}

/// Ask one loopback port's `/health`.
pub fn probe(port: u16) -> State {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let mut stream = match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return State::Down(short(&error.to_string())),
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let request = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUser-Agent: atlas-launcher\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        port
    );
    if let Err(error) = stream.write_all(request.as_bytes()) {
        return State::Down(short(&error.to_string()));
    }
    let mut response = [0u8; 2048];
    let read = match stream.read(&mut response) {
        Ok(read) => read,
        Err(error) => return State::Down(short(&error.to_string())),
    };
    match status_of(&response[..read]) {
        Some(200) => State::Up,
        Some(503) => State::Loading,
        Some(code) => State::Down(format!("the server answered HTTP {}", code)),
        None => State::Down("the answer was not HTTP".to_string()),
    }
}

/// The status code in the first line of an HTTP response.
pub fn status_of(response: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(response);
    let first = text.lines().next()?;
    let mut parts = first.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

/// Wait for the port to answer `200`.
///
/// `probe` is a parameter so a test can drive the wait with a scripted answer and
/// no socket; the app passes [`probe`]. `report` is called with every attempt, so
/// the caller decides how often to print (the app prints the first one and then
/// once every five seconds while the weights load).
///
/// Returns the number of attempts it took, or the last state as a reason.
///
/// `abort` is asked between attempts: it is how a stop request (or a supervisor
/// that has given up) ends the wait early instead of leaving the user watching a
/// port that will never answer.
pub fn wait(
    port: u16,
    deadline: Duration,
    interval: Duration,
    probe: &mut dyn FnMut(u16) -> State,
    report: &mut dyn FnMut(u32, &State),
    abort: &mut dyn FnMut() -> bool,
) -> Result<u32, State> {
    let started = Instant::now();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let state = probe(port);
        report(attempts, &state);
        if state.is_up() {
            return Ok(attempts);
        }
        if started.elapsed() >= deadline || abort() {
            return Err(state);
        }
        // Never sleep past the deadline: the wait is bounded by the clock, not by
        // the number of attempts.
        let left = deadline.saturating_sub(started.elapsed());
        std::thread::sleep(interval.min(left.max(Duration::from_millis(1))));
    }
}

fn short(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 80 {
        return trimmed.to_string();
    }
    trimmed.chars().take(80).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `/health` endpoint: a real loopback socket, a scripted sequence of
    /// answers. This is the "health wait with a fake endpoint" the brief asks for.
    ///
    /// The accept loop is non-blocking and stops when the fixture is dropped, so a
    /// test that makes fewer requests than the script has answers cannot leave a
    /// thread waiting on a connection that never comes.
    struct FakeEndpoint {
        port: u16,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeEndpoint {
        fn start(answers: Vec<&'static str>) -> FakeEndpoint {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).expect("a non-blocking listener");
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let watching = stop.clone();
            let handle = std::thread::spawn(move || {
                let mut left: std::collections::VecDeque<&'static str> = answers.into();
                while !watching.load(std::sync::atomic::Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if let Some(answer) = left.pop_front() {
                                let _ = stream.write_all(answer.as_bytes());
                                let _ = stream.flush();
                            }
                        }
                        Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            FakeEndpoint {
                port,
                stop,
                handle: Some(handle),
            }
        }
    }

    impl Drop for FakeEndpoint {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    const LOADING: &str = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 23\r\n\r\n{\"status\":\"loading\"}\n\n";
    const READY: &str = "HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n{\"status\":\"ok\"}\n";

    #[test]
    fn a_status_line_is_read_and_a_non_http_answer_is_not() {
        assert_eq!(status_of(READY.as_bytes()), Some(200));
        assert_eq!(status_of(LOADING.as_bytes()), Some(503));
        assert_eq!(status_of(b"SSH-2.0-OpenSSH\r\n"), None);
        assert_eq!(status_of(b""), None);
        assert_eq!(status_of(b"HTTP/1.1 abc\r\n"), None);
    }

    #[test]
    fn the_wait_returns_once_the_fake_endpoint_is_ready() {
        // Two 503s then a 200: the wait must come back on the third attempt and
        // must have reported every attempt.
        let fake = FakeEndpoint::start(vec![LOADING, LOADING, READY]);
        let mut seen: Vec<String> = Vec::new();
        let attempts = wait(
            fake.port,
            Duration::from_secs(10),
            Duration::from_millis(5),
            &mut probe,
            &mut |_, state| seen.push(state.text()),
            &mut || false,
        )
        .unwrap();
        assert_eq!(attempts, 3, "seen: {:?}", seen);
        assert_eq!(seen.len(), 3);
        assert!(seen[0].contains("still loading"), "{:?}", seen);
        assert!(seen[2].contains("answering"), "{:?}", seen);
    }

    #[test]
    fn a_port_that_never_answers_is_reported_with_the_last_reason() {
        let fake = FakeEndpoint::start(vec![LOADING, LOADING]);
        let mut attempts = 0u32;
        let error = wait(
            fake.port,
            Duration::from_millis(60),
            Duration::from_millis(10),
            &mut probe,
            &mut |count, _| attempts = count,
            &mut || false,
        )
        .unwrap_err();
        assert!(attempts >= 2, "attempts: {}", attempts);
        // Either "still loading" (the fake answered) or a refused connection (the
        // fake had run out of answers and dropped its listener's client).
        assert!(!error.is_up());
    }

    #[test]
    fn a_closed_port_is_down_with_a_reason_and_not_a_hang() {
        // Port 9 on loopback: nothing listens, so the connect is refused at once.
        let state = probe(9);
        match state {
            State::Down(reason) => assert!(!reason.is_empty()),
            other => panic!("expected Down, got {:?}", other),
        }
    }

    #[test]
    fn the_wait_ends_at_once_when_the_caller_asks_it_to() {
        // A model server that is still "loading" forever must not hold the boot:
        // a stop request (or a supervisor that gave up) ends the wait.
        let fake = FakeEndpoint::start(vec![LOADING; 8]);
        let started = Instant::now();
        let ticks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = ticks.clone();
        let asking = ticks.clone();
        let error = wait(
            fake.port,
            Duration::from_secs(10),
            Duration::from_millis(10),
            &mut probe,
            &mut |_, _| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            &mut || asking.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        )
        .unwrap_err();
        assert!(
            ticks.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "the wait never ran"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the wait ignored the abort: {:?}",
            started.elapsed()
        );
        assert!(!error.is_up());
    }

    #[test]
    fn the_wait_is_bounded_by_the_clock_not_by_the_probe() {
        let started = Instant::now();
        let error = wait(
            9,
            Duration::from_millis(80),
            Duration::from_millis(20),
            &mut probe,
            &mut |_, _| {},
            &mut || false,
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert!(!error.is_up());
    }
}
