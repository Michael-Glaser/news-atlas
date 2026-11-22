//! `schedule` — who is due, at one instant.
//!
//! Decisions: the clock a check reads ([`Clock`]), that the scheduler can be
//! switched off ([`Scheduler::set_enabled`]) — off means nothing runs on its own,
//! not that nothing can run — and what "due" means: active, with a cadence, and
//! either never checked or `cadence_minutes` past its last check.
//!
//! It does not: run anything, hold a thread, or sleep. There is no timer in this
//! crate: [`Scheduler::due`] is a pure function of the monitors and an instant, and
//! [`crate::service::MonitorService::tick`] is what a caller calls when it wants
//! the answer. That is why a fake clock is a test and not a rewrite — see
//! [`FixedClock`].
//!
//! The cadence lives on the monitor row (`monitors.cadence_minutes`), so the
//! schedule survives a restart; `0` means manual only, and the user's button always
//! works whatever the scheduler is doing.

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::spec::Monitor;

/// Seconds since the epoch — the one clock a check reads.
pub trait Clock: Send + Sync {
    fn now_epoch(&self) -> i64;
}

/// The wall clock, through `atlas-store`'s clock so the format is the same one the
/// archive already stores.
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock
    }
}

impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock
    }
}

impl Clock for SystemClock {
    fn now_epoch(&self) -> i64 {
        atlas_store::clock::epoch_secs_now()
    }
}

/// A clock a test (or a replay) sets by hand.
#[derive(Debug)]
pub struct FixedClock {
    secs: Mutex<i64>,
}

impl FixedClock {
    pub fn new(secs: i64) -> FixedClock {
        FixedClock {
            secs: Mutex::new(secs),
        }
    }

    /// Move the clock forward (or back); returns the new instant.
    pub fn set(&self, secs: i64) -> i64 {
        let mut held = match self.secs.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *held = secs;
        *held
    }

    /// Move the clock forward by `secs`.
    pub fn advance(&self, secs: i64) -> i64 {
        let mut held = match self.secs.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *held = held.saturating_add(secs);
        *held
    }
}

impl Clock for FixedClock {
    fn now_epoch(&self) -> i64 {
        match self.secs.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Whether the scheduler is allowed to start checks, and which ones are due.
///
/// Constructed on and off; `Scheduler::new(false)` is the "switched off" state a
/// user can leave it in, and it survives because the server keeps one instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheduler {
    enabled: bool,
}

impl Scheduler {
    pub fn new(enabled: bool) -> Scheduler {
        Scheduler { enabled }
    }

    /// On: the scheduler may ask for due monitors.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Switch it off (a user setting, a maintenance window, a test). Off means
    /// [`Scheduler::due`] answers empty — a button still works.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// The monitors due at `now`, in id order (which is the order the store
    /// returned, i.e. the order a "check now" pass would take).
    ///
    /// An empty answer means one of three things — switched off, nothing active, or
    /// nothing at its cadence — and [`Tick`] says which.
    pub fn due(&self, monitors: &[Monitor], now: i64) -> Vec<Monitor> {
        if !self.enabled {
            return Vec::new();
        }
        monitors
            .iter()
            .filter(|monitor| monitor.is_due(now))
            .cloned()
            .collect()
    }
}

impl Default for Scheduler {
    fn default() -> Scheduler {
        Scheduler::new(true)
    }
}

/// What one tick of the scheduler found and did.
#[derive(Clone, Debug)]
pub struct Tick {
    /// Was the scheduler switched on for this tick?
    pub enabled: bool,
    /// The instant the tick read, ISO, so a log line can be read without a clock.
    pub now: String,
    /// Monitor ids that were due.
    pub due: Vec<i64>,
    /// One report per check that started (empty when the tick only asked).
    pub ran: Vec<crate::run::RunReport>,
    /// A sentence about what the tick could not do — switched off, or a store that
    /// would not answer.
    pub note: String,
}

impl Tick {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "now": self.now,
            "due": self.due,
            "ran": self.ran.iter().map(|report| report.to_json()).collect::<Vec<_>>(),
            "new_alerts": self.ran.iter().map(|report| report.new_alerts()).sum::<usize>(),
            "note": self.note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: i64, cadence: i64, last_run: &str) -> Monitor {
        Monitor {
            id,
            name: format!("m{}", id),
            query: "chip export".to_string(),
            cadence_minutes: cadence,
            active: true,
            last_run: last_run.to_string(),
            ..Monitor::default()
        }
    }

    #[test]
    fn a_switched_off_scheduler_has_nothing_due() {
        let monitors = vec![monitor(1, 60, "")];
        let now = atlas_store::parse_epoch("2026-11-05T12:00:00Z");
        assert!(Scheduler::new(false).due(&monitors, now).is_empty());
        assert_eq!(Scheduler::new(true).due(&monitors, now).len(), 1);
    }

    #[test]
    fn a_fake_clock_walks_a_cadence() {
        // 12:00, a monitor checked at 11:45 with a 30 minute cadence: 15 minutes in.
        let clock = FixedClock::new(atlas_store::parse_epoch("2026-11-05T12:00:00Z"));
        let scheduler = Scheduler::new(true);
        let monitors = vec![monitor(1, 30, "2026-11-05T11:45:00Z")];
        assert!(scheduler.due(&monitors, clock.now_epoch()).is_empty());
        // 12:14 — one minute short, still not due.
        clock.advance(14 * 60);
        assert!(scheduler.due(&monitors, clock.now_epoch()).is_empty());
        // 12:15 — exactly the cadence.
        clock.advance(60);
        assert_eq!(scheduler.due(&monitors, clock.now_epoch()).len(), 1);
        // A monitor that has never run is due at any instant.
        let fresh = vec![monitor(2, 30, "")];
        assert_eq!(scheduler.due(&fresh, clock.now_epoch()).len(), 1);
    }

    #[test]
    fn only_active_monitors_with_a_cadence_are_due() {
        let now = atlas_store::parse_epoch("2026-11-05T12:00:00Z");
        let scheduler = Scheduler::new(true);
        let manual = monitor(1, 0, "");
        let mut paused = monitor(2, 60, "");
        paused.active = false;
        assert!(scheduler.due(&[manual, paused], now).is_empty());
    }

    #[test]
    fn switching_it_back_on_restores_the_schedule() {
        let mut scheduler = Scheduler::new(true);
        let monitors = vec![monitor(1, 60, "")];
        let now = 1_800_000_000;
        scheduler.set_enabled(false);
        assert!(scheduler.due(&monitors, now).is_empty());
        scheduler.set_enabled(true);
        assert_eq!(scheduler.due(&monitors, now).len(), 1);
    }
}
