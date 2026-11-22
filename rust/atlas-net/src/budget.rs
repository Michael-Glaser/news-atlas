//! The one clock a fetch is allowed to run against.
//!
//! A fetch is not one request: it is a request plus up to [`crate::MAX_REDIRECTS`]
//! hops, and every hop is a separate `ureq` call with its own global timeout. Left
//! to the transport, five hops bought five fresh budgets — up to six times
//! [`crate::TIMEOUT_TOTAL`] for one link, which is not what that constant promised
//! and not what a tool call can afford to spend.
//!
//! [`Budget`] is that promise, started once per fetch: every hop is handed only
//! what is left of it, and when nothing is left the next hop is never requested.
//!
//! It does not interrupt a request, log, retry or measure anything: it answers
//! "how much time is left", and the caller decides what to do with the answer.

use std::time::{Duration, Instant};

/// The time one fetch may spend, counted from the first gate to the last byte.
pub struct Budget {
    total: Duration,
    started: Instant,
}

impl Budget {
    /// Start the clock for a fetch that may take `total` in all. A zero total is
    /// legal and means "already spent": the fetch fails before it opens a socket.
    pub fn new(total: Duration) -> Budget {
        Budget {
            total,
            started: Instant::now(),
        }
    }

    /// What the whole fetch was given.
    pub fn total(&self) -> Duration {
        self.total
    }

    /// How much of it has been used so far.
    pub fn spent(&self) -> Duration {
        self.started.elapsed()
    }

    /// What is left. Never negative: an exhausted budget is zero, and the caller
    /// must not treat zero as "unlimited" (it is the opposite).
    pub fn remaining(&self) -> Duration {
        self.total.saturating_sub(self.spent())
    }

    /// True when there is nothing left to spend on another hop.
    pub fn is_spent(&self) -> bool {
        self.remaining().is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_budget_has_almost_all_of_its_time_left() {
        let budget = Budget::new(Duration::from_secs(20));
        assert_eq!(budget.total(), Duration::from_secs(20));
        assert!(!budget.is_spent());
        // Nothing is spent in the first instant, and the remainder never exceeds
        // the total (which would mean the subtraction had gone the wrong way).
        assert!(budget.remaining() <= Duration::from_secs(20));
        assert!(budget.remaining() > Duration::from_secs(19));
    }

    #[test]
    fn a_budget_runs_out_and_never_goes_negative() {
        let budget = Budget::new(Duration::from_millis(30));
        std::thread::sleep(Duration::from_millis(60));
        assert!(budget.is_spent());
        assert_eq!(budget.remaining(), Duration::ZERO);
        // Spending more time must not wrap into a healthy-looking remainder.
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(budget.remaining(), Duration::ZERO);
        assert!(budget.spent() >= Duration::from_millis(60));
    }

    #[test]
    fn a_zero_budget_is_spent_before_the_first_socket() {
        let budget = Budget::new(Duration::ZERO);
        assert!(budget.is_spent());
        assert_eq!(budget.remaining(), Duration::ZERO);
    }
}
