//! The clock seam: one number, read where a window is applied.
//!
//! [`Clock::now`] is the only way this crate learns the time, so a test can put
//! the whole pipeline on a fixed day and a fixture date is either inside or
//! outside the window for a reason that can be read in the test.
//!
//! It does not: format, parse, or decide a window. Those are `atlas-dates`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Where "now" comes from, in epoch seconds.
pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now(&self) -> i64;
}

/// The machine's clock, clamped at zero if it predates the epoch.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Clock for System {
    fn now(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(delta) => delta.as_secs() as i64,
            Err(_) => 0,
        }
    }
}

/// A clock that stands still — tests, and one reproducible measurement.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fixed(pub i64);

impl Clock for Fixed {
    fn now(&self) -> i64 {
        self.0
    }
}
