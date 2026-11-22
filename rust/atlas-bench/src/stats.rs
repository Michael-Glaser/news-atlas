//! One measurement: a name, its samples, and the percentiles a report quotes.
//!
//! Decisions: how a path is timed (warm-up, then `runs` samples, the clock around
//! the whole unit of work), and how a percentile is picked from a sorted sample
//! (nearest-rank, never interpolated — a p95 that no run reached is not a
//! measurement).
//!
//! It does not: print, compare against a budget, or decide what is slow. Those are
//! [`crate::report`], [`crate::budgets`] and the reader's job.

use std::time::{Duration, Instant};

/// How many timed runs one path gets, and how many warm-up runs precede them.
pub const RUNS: usize = 30;
pub const WARMUP: usize = 5;

/// The samples of one path.
#[derive(Clone, Debug)]
pub struct Samples {
    /// Stable id: what [`crate::budgets`] keys on.
    pub path: &'static str,
    /// Every timed run, in the order it happened (before sorting).
    pub times: Vec<Duration>,
    /// Response size of the last run, when a path has one.
    pub bytes: usize,
}

impl Samples {
    pub fn new(path: &'static str, times: Vec<Duration>) -> Samples {
        Samples { path, times, bytes: 0 }
    }

    pub fn with_bytes(mut self, bytes: usize) -> Samples {
        self.bytes = bytes;
        self
    }

    pub fn runs(&self) -> usize {
        self.times.len()
    }

    fn sorted(&self) -> Vec<Duration> {
        let mut copy = self.times.clone();
        copy.sort_unstable();
        copy
    }

    /// Nearest-rank percentile: the smallest sample at or above `p` percent.
    pub fn percentile(&self, p: f64) -> Duration {
        let sorted = self.sorted();
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
    }

    pub fn p50(&self) -> Duration {
        self.percentile(50.0)
    }

    pub fn p95(&self) -> Duration {
        self.percentile(95.0)
    }

    pub fn min(&self) -> Duration {
        self.sorted().first().copied().unwrap_or_default()
    }

    pub fn max(&self) -> Duration {
        self.sorted().last().copied().unwrap_or_default()
    }

    pub fn mean(&self) -> Duration {
        if self.times.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.times.iter().sum();
        total / self.times.len() as u32
    }

    /// Milliseconds, the unit every table here uses.
    pub fn ms(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1000.0
    }
}

/// Time one unit of work: `warmup` untimed runs, then [`RUNS`] timed ones.
///
/// The closure returns the path's own answer (the response, the rows) and its
/// size in bytes, so a caller can assert the work really happened instead of
/// timing an error page. Returns the samples and the last answer.
pub fn measure<T, F>(path: &'static str, work: F) -> (Samples, T)
where
    F: FnMut() -> (T, usize),
{
    measure_with(path, WARMUP, RUNS, work)
}

/// [`measure`] with the run counts spelled out (a slow path gets fewer runs).
pub fn measure_with<T, F>(path: &'static str, warmup: usize, runs: usize, mut work: F) -> (Samples, T)
where
    F: FnMut() -> (T, usize),
{
    for _ in 0..warmup {
        let _ = work();
    }
    let mut times: Vec<Duration> = Vec::with_capacity(runs.max(1));
    let mut answer: Option<T> = None;
    let mut bytes = 0usize;
    for _ in 0..runs.max(1) {
        let started = Instant::now();
        let (value, size) = work();
        times.push(started.elapsed());
        bytes = size;
        answer = Some(value);
    }
    let samples = Samples {
        path,
        times,
        bytes,
    };
    (
        samples,
        answer.expect("at least one run always happens"),
    )
}

/// Time one unit of work that has no answer to hand back.
pub fn time_only<F>(path: &'static str, mut work: F) -> Samples
where
    F: FnMut() -> usize,
{
    let (samples, _) = measure(path, move || {
        let size = work();
        ((), size)
    });
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_percentile_is_a_sample_that_really_happened() {
        let times: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let samples = Samples::new("test", times);
        assert_eq!(samples.runs(), 100);
        assert_eq!(samples.p50(), Duration::from_millis(50));
        assert_eq!(samples.p95(), Duration::from_millis(95));
        assert_eq!(samples.min(), Duration::from_millis(1));
        assert_eq!(samples.max(), Duration::from_millis(100));
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let samples = Samples::new("one", vec![Duration::from_millis(7)]);
        assert_eq!(samples.p50(), Duration::from_millis(7));
        assert_eq!(samples.p95(), Duration::from_millis(7));
    }

    #[test]
    fn an_empty_sample_set_is_zero_and_does_not_panic() {
        let samples = Samples::new("empty", Vec::new());
        assert_eq!(samples.p95(), Duration::ZERO);
        assert_eq!(samples.mean(), Duration::ZERO);
    }

    #[test]
    fn measure_runs_the_work_the_declared_number_of_times() {
        let calls = std::cell::Cell::new(0usize);
        let (samples, last) = measure_with("counted", 2, 6, || {
            calls.set(calls.get() + 1);
            (42, calls.get())
        });
        assert_eq!(samples.runs(), 6);
        assert_eq!(last, 42, "the answer of the last timed run comes back");
        assert_eq!(calls.get(), 8, "two warm-ups plus six timed runs");
    }
}
