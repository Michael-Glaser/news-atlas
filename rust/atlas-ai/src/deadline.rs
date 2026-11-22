//! One deadline for one exchange, and what a socket timeout looks like.
//!
//! Decisions: a request gets one wall-clock budget and every read and write asks
//! how much of it is left ([`Deadline::remaining`]); a zero or negative remainder
//! is a [`AiError::Timeout`] at the point of use, never a silent retry. `timed_out`
//! is the one place that says which `io::ErrorKind`s mean "the clock", so a socket
//! error cannot be mistaken for one.
//!
//! It does not: set a timeout on a socket, sleep, retry, or decide the budget — the
//! caller passes it and [`crate::http1`] applies it.

use std::time::{Duration, Instant};

/// The wall clock this exchange must be finished by.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Instant,
}

impl Deadline {
    pub fn new(budget: Duration) -> Deadline {
        Deadline {
            at: Instant::now() + budget,
        }
    }

    /// Time left, or `None` when the deadline has passed.
    pub fn remaining(&self) -> Option<Duration> {
        let now = Instant::now();
        if now >= self.at {
            None
        } else {
            Some(self.at - now)
        }
    }
}

pub(crate) fn timed_out(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}
