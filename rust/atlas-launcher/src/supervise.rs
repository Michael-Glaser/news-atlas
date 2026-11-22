//! The model server's supervisor: watch the child, restart it **once** with
//! backoff, then give up and say so.
//!
//! `llama-server` is the one process ATLAS does not write, and it can die: a bad
//! quantization, an out-of-memory load, a user killing it. The rule is deliberately
//! small and never turns into a flock of retries:
//!
//! * the child is polled (`try_wait`), never `wait`ed on forever, so a stop request
//!   is honoured within [`POLL`];
//! * a first exit is reported, followed by [`BACKOFF`] and **one** restart;
//! * a second exit is reported and the supervisor stops. A crash loop that hides
//!   itself is worse than a clear "the model server is down".
//! * the child is stopped even if this thread unwinds ([`Armed`]), and the boot's
//!   liveness flag goes down with the thread ([`Alive`]): a panic in the supervisor
//!   is not a way to leave a server listening or to make the boot wait four minutes
//!   for a port nothing is on.
//!
//! The loop is written against [`ModelProcess`], so a test drives it with a
//! scripted child and a 30 ms backoff, and `atlas start` drives it with the real
//! `llama-server`. Nothing here decides *what* to spawn — that is
//! [`crate::llama`] — and nothing here prints: events go to the caller, so the
//! wording stays in one place.

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::child;
use crate::report::Printer;

/// How many times a dead model server is brought back. One: a second death is a
/// fact about this machine, not a hiccup.
pub const MAX_RESTARTS: u32 = 1;

/// How long to wait before the single restart.
pub const BACKOFF: Duration = Duration::from_secs(3);

/// How often the child's status is checked while nothing happens.
pub const POLL: Duration = Duration::from_millis(250);

/// How long a stop may take before the child is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// The restart policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub max_restarts: u32,
    pub backoff: Duration,
    pub poll: Duration,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            max_restarts: MAX_RESTARTS,
            backoff: BACKOFF,
            poll: POLL,
        }
    }
}

/// Something worth telling the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The child is gone, with what the operating system said about it.
    Died { status: String },
    /// The single restart is about to happen, after a pause. `of` is the policy in
    /// force, not the constant: a test (or a future policy) may allow more than one.
    Restarting { attempt: u32, of: u32, after: Duration },
    /// The child was started again.
    Restarted { attempt: u32 },
    /// It died again and will not be restarted.
    GaveUp { restarts: u32, status: String },
    /// The restart itself failed (the binary went away, the port is held).
    RestartFailed { detail: String },
}

impl Event {
    /// The sentence a user reads, with no colour and no article text.
    pub fn line(&self) -> String {
        match self {
            Event::Died { status } => format!("the model server {}", status),
            Event::Restarting { attempt, of, after } => format!(
                "restarting the model server in {}s (restart {} of {})",
                after.as_secs_f64().ceil() as u64,
                attempt,
                of
            ),
            Event::Restarted { attempt } => {
                format!("the model server is back (restart {})", attempt)
            }
            Event::GaveUp { restarts, status } => format!(
                "the model server {} again after {} restart{} and will not be restarted \
                 — ATLAS keeps serving without its local model (see data/llama.log)",
                status,
                restarts,
                if *restarts == 1 { "" } else { "s" }
            ),
            Event::RestartFailed { detail } => format!(
                "the model server could not be restarted: {}. ATLAS keeps serving without it",
                detail
            ),
        }
    }
}

/// How the supervisor ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Asked to stop; the child was stopped by this module.
    Stopped,
    /// It died for good; nothing was left running.
    GaveUp { restarts: u32 },
    /// The supervisor itself could not read the child.
    Failed(String),
}

/// Is a model server supposed to be running?
///
/// Shared with the boot sequence so its health wait can stop early when the
/// supervisor has given up, instead of watching a port nothing will ever answer
/// on. `true` while a child exists, `false` the moment the supervisor ends.
#[derive(Clone)]
pub struct Liveness {
    flag: Arc<AtomicBool>,
}

impl Default for Liveness {
    fn default() -> Liveness {
        Liveness::new()
    }
}

impl Liveness {
    pub fn new() -> Liveness {
        Liveness {
            flag: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn alive(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn set(&self, alive: bool) {
        self.flag.store(alive, Ordering::SeqCst);
    }

    /// A liveness flag that already says "nothing is running" — the state the boot
    /// sequence's health wait watches, without starting a supervisor or a child.
    #[cfg(test)]
    pub fn gone() -> Liveness {
        let liveness = Liveness::new();
        liveness.set(false);
        liveness
    }
}

/// One child a supervisor can watch.
pub trait ModelProcess: Send {
    /// `Some(status)` when it has exited, `None` while it runs.
    fn try_wait(&mut self) -> Result<Option<String>, String>;
    /// Stop it and describe what happened. Called at most once.
    fn stop(&mut self) -> String;
}

/// A real process, spawned through [`crate::child`].
pub struct SpawnedChild {
    child: Child,
    pub pid: u32,
}

impl SpawnedChild {
    pub fn new(child: Child) -> SpawnedChild {
        let pid = child.id();
        SpawnedChild { child, pid }
    }
}

impl ModelProcess for SpawnedChild {
    fn try_wait(&mut self) -> Result<Option<String>, String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(format!("exited with {}", status))),
            Ok(None) => Ok(None),
            Err(error) => Err(format!("cannot read its status: {}", error)),
        }
    }

    fn stop(&mut self) -> String {
        child::terminate(&mut self.child, STOP_GRACE)
    }
}

/// Watch one child until it is stopped or it dies for good.
///
/// `spawn` is called only for the restart, never for the first child: the caller
/// starts that one itself so a failure is reported to the user immediately and
/// with the real error.
pub fn run(
    first: Box<dyn ModelProcess>,
    policy: &Policy,
    mut spawn: Box<dyn FnMut() -> Result<Box<dyn ModelProcess>, String>>,
    stop: &Receiver<()>,
    report: &mut dyn FnMut(Event),
    liveness: &Liveness,
) -> Outcome {
    let mut child = Armed::new(first);
    // The flag the boot's health wait watches goes down with this function, on every
    // way out — including the unwinding one. Without it a panicked supervisor left
    // the flag up and the boot waited its whole deadline for a port nothing would
    // ever answer on.
    let _alive = Alive::new(liveness);
    liveness.set(true);
    let mut restarts = 0u32;
    loop {
        match child.watch() {
            Ok(Some(status)) => {
                if restarts >= policy.max_restarts {
                    // Only the give-up line here: repeating the death first would
                    // print the same exit status twice in a row.
                    child.died();
                    report(Event::GaveUp { restarts, status });
                    liveness.set(false);
                    return Outcome::GaveUp { restarts };
                }
                report(Event::Died {
                    status: status.clone(),
                });
                restarts += 1;
                report(Event::Restarting {
                    attempt: restarts,
                    of: policy.max_restarts,
                    after: policy.backoff,
                });
                if waiting(stop, policy.backoff) {
                    // A stop arrived during the backoff: nothing is running, so
                    // there is nothing to stop, but the answer is still "stopped".
                    child.died();
                    liveness.set(false);
                    return Outcome::Stopped;
                }
                match spawn() {
                    Ok(next) => {
                        child.replace(next);
                        liveness.set(true);
                        report(Event::Restarted { attempt: restarts });
                    }
                    Err(detail) => {
                        child.died();
                        report(Event::RestartFailed { detail });
                        liveness.set(false);
                        return Outcome::GaveUp { restarts };
                    }
                }
            }
            Ok(None) => {
                if waiting(stop, policy.poll) {
                    child.stop();
                    liveness.set(false);
                    return Outcome::Stopped;
                }
            }
            Err(detail) => {
                child.stop();
                liveness.set(false);
                return Outcome::Failed(detail);
            }
        }
    }
}

/// The liveness flag, cleared when the supervisor ends however it ends.
///
/// `run` sets the flag false on each of its own returns; this is the guard for the
/// path it does not own — a panic in this thread — so the boot's health wait cannot
/// sit out its four-minute deadline for a port that nothing is listening on.
struct Alive(Liveness);

impl Alive {
    fn new(liveness: &Liveness) -> Alive {
        Alive(liveness.clone())
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// The child [`run`] owes a stop to, and the promise that it gets one even when
/// this thread unwinds.
///
/// Every path `run` owns stops the child or records that it died by itself. A path
/// it does not own is a panic — in `report` (a poisoned lock in the printer) or in a
/// [`ModelProcess`] implementation — and unwinding past `run` used to leave the
/// model server listening after ATLAS had printed that nothing it started was still
/// running. It is the same defect as the supervisor thread that never started, one
/// unwinding further: dropping a `std::process::Child` does not stop anything on any
/// platform. This guard is what makes that true for the last path too.
struct Armed {
    child: Box<dyn ModelProcess>,
    /// Set once this struct no longer owes the child a stop: it was stopped here, or
    /// it exited by itself and `try_wait` already reaped it.
    settled: bool,
}

impl Armed {
    fn new(child: Box<dyn ModelProcess>) -> Armed {
        Armed {
            child,
            settled: false,
        }
    }

    fn watch(&mut self) -> Result<Option<String>, String> {
        self.child.try_wait()
    }

    /// Stop it and record that the debt is paid.
    fn stop(&mut self) -> String {
        self.settled = true;
        self.child.stop()
    }

    /// It exited by itself: `try_wait` reaped it, so there is nothing to stop.
    fn died(&mut self) {
        self.settled = true;
    }

    /// A new child took the dead one's place, and the debt moves to it.
    fn replace(&mut self, next: Box<dyn ModelProcess>) {
        self.child = next;
        self.settled = false;
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        if !self.settled {
            // This runs while the thread unwinds; a panic in `stop` here would abort
            // the process, so its sentence is dropped rather than printed.
            let _ = self.child.stop();
        }
    }
}

/// Poll the stop channel for `span`. `true` means "stop now".
fn waiting(stop: &Receiver<()>, span: Duration) -> bool {
    match stop.recv_timeout(span) {
        Ok(()) => true,
        Err(RecvTimeoutError::Timeout) => false,
        // The caller is gone: nobody can ask again, so stop.
        Err(RecvTimeoutError::Disconnected) => true,
    }
}

/// A running supervisor on its own thread.
///
/// The child lives in a slot both this handle and the supervisor thread can reach,
/// because there are two ways a supervisor can fail to exist and only one of them
/// has a thread to ask:
///
/// * the thread ran and ended (a stop, a give-up) — the thread already stopped the
///   child it was given, and [`Handle::stop`] joins it;
/// * the thread was **never created** (`Builder::spawn` refused: out of memory, out
///   of threads). The child was moved into the closure, the closure was dropped, and
///   dropping a `std::process::Child` does not stop the process on any platform —
///   so the model server kept running after ATLAS had printed "Nothing it started is
///   still running". The slot is what lets `stop()` reap it anyway.
pub struct Handle {
    stop: Option<Sender<()>>,
    join: Option<std::thread::JoinHandle<Outcome>>,
    /// Where the child was before the supervisor thread took it.
    slot: SharedProcess,
}

impl Handle {
    /// Ask the supervisor to stop, wait for it, and report how it ended.
    ///
    /// A second call finds nothing left to join and returns `None`.
    pub fn stop(&mut self) -> Option<Outcome> {
        match self.stop.take() {
            Some(sender) => {
                let _ = sender.send(());
                self.join.take().map(|join| {
                    join.join().unwrap_or_else(|_| {
                        // The thread unwound. `Armed` stopped its child on the way
                        // out, so "nothing ATLAS started is still running" still
                        // holds; what does not hold is that this was an ordinary
                        // stop, and the boot's last lines should say so.
                        Outcome::Failed("the supervisor ended unexpectedly".to_string())
                    })
                })
            }
            None if self.join.is_none() => {
                // The supervisor thread never started. Whatever is still in the slot
                // is nobody's child now, and this is the only place that can stop it.
                let waiting = self.slot.lock().ok().and_then(|mut slot| slot.take());
                match waiting {
                    Some(mut child) => {
                        child.stop();
                        Some(Outcome::Stopped)
                    }
                    None => None,
                }
            }
            None => None,
        }
    }

    /// Is the supervisor still running?
    pub fn running(&self) -> bool {
        self.stop.is_some()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.stop.is_some() || self.join.is_none() {
            let _ = self.stop();
        }
    }
}

/// The child, in a slot the supervisor thread and the [`Handle`] share.
pub type SharedProcess = Arc<Mutex<Option<Box<dyn ModelProcess>>>>;

/// Start watching a child that is already running, on its own thread.
///
/// Returns the handle that stops it and the flag the boot sequence watches. The
/// child must be spawned by the caller *before* this call, and this call must
/// happen *before* the health wait: a model server that dies while the weights are
/// loading has to be restarted, not waited on for four minutes.
pub fn start(
    first: Box<dyn ModelProcess>,
    policy: Policy,
    respawn: Box<dyn FnMut() -> Result<Box<dyn ModelProcess>, String> + Send + 'static>,
    printer: Arc<Printer>,
) -> (Handle, Liveness) {
    let (sender, receiver) = std::sync::mpsc::channel::<()>();
    let liveness = Liveness::new();
    let watched = liveness.clone();
    let slot: SharedProcess = Arc::new(Mutex::new(Some(first)));
    let inside = slot.clone();
    let join = std::thread::Builder::new()
        .name("atlas-supervisor".to_string())
        .spawn(move || {
            // The thread owns the child from here on; if the slot is empty somebody
            // else already stopped it, and there is nothing to watch.
            let first = match inside.lock().ok().and_then(|mut slot| slot.take()) {
                Some(first) => first,
                None => {
                    watched.set(false);
                    return Outcome::Stopped;
                }
            };
            let mut report = |event: Event| {
                let line = event.line();
                match event {
                    Event::Died { .. } | Event::GaveUp { .. } | Event::RestartFailed { .. } => {
                        printer.warn(line)
                    }
                    _ => printer.note(line),
                }
            };
            run(first, &policy, respawn, &receiver, &mut report, &watched)
        })
        .ok();
    wire(slot, join, sender, liveness)
}

/// Assemble the [`Handle`] around a thread that may or may not exist.
///
/// Split out from [`start`] so the "the thread was never created" case has one
/// implementation and a test can drive it with a real child: `None` is exactly what
/// `Builder::spawn` returns when it refuses.
fn wire(
    slot: SharedProcess,
    join: Option<std::thread::JoinHandle<Outcome>>,
    sender: Sender<()>,
    liveness: Liveness,
) -> (Handle, Liveness) {
    if join.is_none() {
        // Nothing is watching: the boot must stop waiting for a port, and `stop()`
        // above is the last chance to reap the child.
        liveness.set(false);
    }
    (
        Handle {
            stop: join.as_ref().map(|_| sender),
            join,
            slot,
        },
        liveness,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    /// A scripted child: it "runs" for `polls_until_exit` checks, then exits.
    struct Scripted {
        polls_until_exit: u32,
        polls: u32,
        stopped: std::sync::Arc<AtomicU32>,
    }

    impl ModelProcess for Scripted {
        fn try_wait(&mut self) -> Result<Option<String>, String> {
            self.polls += 1;
            if self.polls >= self.polls_until_exit {
                Ok(Some("exited with exit status: 1".to_string()))
            } else {
                Ok(None)
            }
        }
        fn stop(&mut self) -> String {
            self.stopped.fetch_add(1, Ordering::SeqCst);
            "stopped".to_string()
        }
    }

    fn scripted(polls: u32, stopped: &std::sync::Arc<AtomicU32>) -> Box<dyn ModelProcess> {
        Box::new(Scripted {
            polls_until_exit: polls,
            polls: 0,
            stopped: stopped.clone(),
        })
    }

    #[test]
    fn a_dead_model_server_is_restarted_once_with_a_backoff_then_left_alone() {
        let stopped = std::sync::Arc::new(AtomicU32::new(0));
        let spawns = std::sync::Arc::new(AtomicU32::new(0));
        let (_sender, receiver) = mpsc::channel::<()>();
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(40),
            poll: Duration::from_millis(2),
        };
        let mut events: Vec<Event> = Vec::new();
        let counter = spawns.clone();
        let respawn_handle = stopped.clone();
        let liveness = Liveness::new();
        let started = Instant::now();
        let outcome = run(
            scripted(1, &stopped),
            &policy,
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(scripted(1, &respawn_handle))
            }),
            &receiver,
            &mut |event| events.push(event),
            &liveness,
        );
        let elapsed = started.elapsed();
        assert_eq!(outcome, Outcome::GaveUp { restarts: 1 });
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "exactly one restart");
        assert!(!liveness.alive(), "the boot sequence must stop waiting");
        assert!(
            elapsed >= Duration::from_millis(40),
            "the backoff was not waited: {:?}",
            elapsed
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 0, "nothing to stop once it is dead");
        assert!(matches!(events[0], Event::Died { .. }), "{:?}", events);
        assert!(
            matches!(events[1], Event::Restarting { after, .. } if after == Duration::from_millis(40)),
            "{:?}",
            events
        );
        assert!(matches!(events[2], Event::Restarted { attempt: 1 }), "{:?}", events);
        assert!(matches!(events.last(), Some(Event::GaveUp { restarts: 1, .. })), "{:?}", events);
        // Two deaths, one "it died" line: the second is the give-up line itself.
        let deaths = events
            .iter()
            .filter(|event| matches!(event, Event::Died { .. }))
            .count();
        assert_eq!(deaths, 1, "{:?}", events);
        // The sentence names the log file and says the app keeps running.
        let line = events.last().unwrap().line();
        assert!(line.contains("data/llama.log"), "{}", line);
        assert!(line.contains("keeps serving"), "{}", line);
    }

    #[test]
    fn a_stop_request_stops_the_child_and_ends_the_supervisor() {
        let stopped = std::sync::Arc::new(AtomicU32::new(0));
        let (sender, receiver) = mpsc::channel::<()>();
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(10),
            poll: Duration::from_millis(5),
        };
        let stopper = stopped.clone();
        let watched = Liveness::new();
        let inner = watched.clone();
        let thread = std::thread::spawn(move || {
            let mut events: Vec<Event> = Vec::new();
            let outcome = run(
                scripted(u32::MAX, &stopper),
                &policy,
                Box::new(|| Err("never".to_string())),
                &receiver,
                &mut |event| events.push(event),
                &inner,
            );
            (outcome, events)
        });
        assert!(watched.alive());
        std::thread::sleep(Duration::from_millis(30));
        sender.send(()).unwrap();
        let (outcome, events) = thread.join().unwrap();
        assert_eq!(outcome, Outcome::Stopped);
        assert!(!watched.alive(), "a stopped supervisor is not alive");
        assert_eq!(stopped.load(Ordering::SeqCst), 1, "the child was stopped exactly once");
        assert!(events.is_empty(), "a quiet stop reports nothing: {:?}", events);
    }

    #[test]
    fn a_restart_that_cannot_spawn_reports_the_reason_and_stops() {
        let stopped = std::sync::Arc::new(AtomicU32::new(0));
        let (_sender, receiver) = mpsc::channel::<()>();
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(1),
            poll: Duration::from_millis(1),
        };
        let mut events: Vec<Event> = Vec::new();
        let liveness = Liveness::new();
        let outcome = run(
            scripted(1, &stopped),
            &policy,
            Box::new(|| Err("bin/llama-server went away".to_string())),
            &receiver,
            &mut |event| events.push(event),
            &liveness,
        );
        assert!(!liveness.alive());
        assert_eq!(outcome, Outcome::GaveUp { restarts: 1 });
        assert!(
            matches!(events.last(), Some(Event::RestartFailed { detail }) if detail.contains("went away")),
            "{:?}",
            events
        );
    }

    #[test]
    fn a_dropped_stop_handle_is_read_as_stop_not_as_a_spin() {
        let stopped = std::sync::Arc::new(AtomicU32::new(0));
        let (sender, receiver) = mpsc::channel::<()>();
        drop(sender);
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(1),
            poll: Duration::from_millis(1),
        };
        let liveness = Liveness::new();
        let outcome = run(
            scripted(u32::MAX, &stopped),
            &policy,
            Box::new(|| Err("never".to_string())),
            &receiver,
            &mut |_| {},
            &liveness,
        );
        assert_eq!(outcome, Outcome::Stopped);
    }

    #[test]
    fn the_restart_line_counts_against_the_policy_in_force_not_the_constant() {
        // A test policy of three must not print "restart 1 of 1": the sentence is
        // read by a user deciding whether to wait or to give up on the model.
        let line = Event::Restarting {
            attempt: 1,
            of: 3,
            after: Duration::from_secs(3),
        }
        .line();
        assert!(line.contains("restart 1 of 3"), "{}", line);
        assert!(line.contains("3s"), "{}", line);
        // The shipped policy is still one.
        assert_eq!(Policy::default().max_restarts, MAX_RESTARTS);
        let default_line = Event::Restarting {
            attempt: 1,
            of: Policy::default().max_restarts,
            after: BACKOFF,
        }
        .line();
        assert!(default_line.contains("restart 1 of 1"), "{}", default_line);
    }

    #[test]
    fn a_liveness_flag_can_say_nothing_is_running_without_a_child() {
        assert!(Liveness::new().alive());
        assert!(!Liveness::gone().alive());
    }

    #[test]
    fn a_child_that_never_dies_is_watched_without_a_restart() {
        let stopped = std::sync::Arc::new(AtomicU32::new(0));
        let spawns = std::sync::Arc::new(AtomicU32::new(0));
        let (sender, receiver) = mpsc::channel::<()>();
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(1),
            poll: Duration::from_millis(2),
        };
        let counter = spawns.clone();
        let stopper = stopped.clone();
        let inner = Liveness::new();
        let thread = std::thread::spawn(move || {
            run(
                scripted(u32::MAX, &stopper),
                &policy,
                Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err("never".to_string())
                }),
                &receiver,
                &mut |_| {},
                &inner,
            )
        });
        std::thread::sleep(Duration::from_millis(25));
        sender.send(()).unwrap();
        assert_eq!(thread.join().unwrap(), Outcome::Stopped);
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_supervisor_thread_that_never_started_still_reaps_its_child() {
        // `Builder::spawn` can refuse (out of memory, out of threads). The child was
        // already spawned by the caller and moved in here, and dropping a
        // `std::process::Child` does not stop the process: before the slot existed,
        // this path kept the model server alive after ATLAS had printed "Nothing it
        // started is still running". `None` is exactly what a refused spawn returns.
        if !std::path::Path::new("/bin/sleep").is_file() {
            return;
        }
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sleep starts");
        let pid = child.id();
        let slot: SharedProcess = Arc::new(Mutex::new(Some(
            Box::new(SpawnedChild::new(child)) as Box<dyn ModelProcess>
        )));
        let (sender, _receiver) = mpsc::channel::<()>();
        let (mut handle, liveness) = wire(slot.clone(), None, sender, Liveness::new());
        assert!(!handle.running(), "a supervisor with no thread is not running");
        assert!(
            !liveness.alive(),
            "the boot's health wait must not wait for a supervisor that does not exist"
        );

        assert_eq!(handle.stop(), Some(Outcome::Stopped));
        // The process is really gone, not merely dropped: `kill -0` fails.
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .env_clear()
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!alive, "pid {} is still running after stop()", pid);
        assert!(slot.lock().unwrap().is_none(), "the slot was emptied");
        // A second stop is not an error, and dropping the handle twice does nothing.
        assert_eq!(handle.stop(), None);
    }

    /// A real child wrapped in a watcher that panics: the stand-in for any panic in
    /// the supervisor thread (a poisoned lock in the printer, a `ModelProcess` that
    /// breaks), which no `unwind` in `run` catches.
    struct Panicking {
        inner: SpawnedChild,
        stops: std::sync::Arc<AtomicU32>,
    }

    impl ModelProcess for Panicking {
        fn try_wait(&mut self) -> Result<Option<String>, String> {
            panic!("the watcher itself broke");
        }

        fn stop(&mut self) -> String {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.inner.stop()
        }
    }

    /// A real child whose polls are counted, so the loop's cadence is measured
    /// rather than read off the constants.
    struct Counted {
        inner: SpawnedChild,
        polls: std::sync::Arc<AtomicU32>,
    }

    impl ModelProcess for Counted {
        fn try_wait(&mut self) -> Result<Option<String>, String> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.inner.try_wait()
        }

        fn stop(&mut self) -> String {
            self.inner.stop()
        }
    }

    fn real_child(program: &str) -> Option<std::process::Child> {
        if !std::path::Path::new(program).is_file() {
            return None;
        }
        Some(
            std::process::Command::new(program)
                .arg(if program.ends_with("sleep") { "30" } else { "" })
                .env_clear()
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?,
        )
    }

    #[test]
    fn a_supervisor_that_panics_stops_its_child_on_the_way_out() {
        // A panic in the supervisor thread used to unwind straight past every
        // `child.stop()` in `run`, and dropping a `std::process::Child` stops
        // nothing: the model server kept listening after ATLAS had closed. Measured
        // before the guard: `stops` stayed 0 and the pid was still in the process
        // table after `Handle::stop` returned.
        let Some(child) = real_child("/bin/sleep") else {
            return;
        };
        let pid = child.id();
        let stops = std::sync::Arc::new(AtomicU32::new(0));
        let (mut handle, liveness) = start(
            Box::new(Panicking {
                inner: SpawnedChild::new(child),
                stops: stops.clone(),
            }),
            Policy {
                max_restarts: 1,
                backoff: Duration::from_millis(1),
                poll: Duration::from_millis(1),
            },
            Box::new(|| Err("never".to_string())),
            Arc::new(Printer::new()),
        );
        // The thread panics on its first poll. The harness reports that on stderr;
        // what it must not do is leave the child running.
        let outcome = handle.stop();
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "the child must be stopped exactly once, on the way out"
        );
        assert!(
            matches!(outcome, Some(Outcome::Failed(_))),
            "a panicked supervisor is not an ordinary stop: {:?}",
            outcome
        );
        assert!(!liveness.alive(), "the boot must not wait for a dead supervisor");
        if crate::tests::ps_program().is_some() {
            assert_eq!(
                crate::tests::wait_for_no_entry(pid),
                None,
                "pid {} is still in the process table after a panic",
                pid
            );
        }
    }

    #[test]
    fn a_live_child_is_polled_on_a_clock_and_never_in_a_spin() {
        // The window's repaint cadence is the desktop's business, but the launcher
        // has the same failure mode one layer down: a supervisor that polls without
        // waiting burns a core for as long as the app runs. This counts the polls a
        // real child sees over ~250 ms at a 25 ms poll: ten looks, not thousands.
        let Some(child) = real_child("/bin/sleep") else {
            return;
        };
        let polls = std::sync::Arc::new(AtomicU32::new(0));
        let counted = Counted {
            inner: SpawnedChild::new(child),
            polls: polls.clone(),
        };
        let (sender, receiver) = mpsc::channel::<()>();
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(10),
            poll: Duration::from_millis(25),
        };
        let thread = std::thread::spawn(move || {
            run(
                Box::new(counted),
                &policy,
                Box::new(|| Err("never".to_string())),
                &receiver,
                &mut |_| {},
                &Liveness::new(),
            )
        });
        std::thread::sleep(Duration::from_millis(250));
        sender.send(()).unwrap();
        assert_eq!(thread.join().unwrap(), Outcome::Stopped);
        let seen = polls.load(Ordering::SeqCst);
        assert!(
            (4..=20).contains(&seen),
            "{} status checks in 250 ms at a 25 ms poll is a spin, not a clock",
            seen
        );
    }

    #[test]
    fn a_crash_loop_costs_one_restart_and_then_silence() {
        // A real child that exits at once is the shape of a crash loop. Measured
        // with the shipped policy shape (one restart, a backoff): the loop is
        // entered twice, waits the backoff, gives up, and the thread ends — so
        // nothing watches, polls or restarts a dead child for the life of the app.
        let Some(first) = real_child("/bin/echo") else {
            return;
        };
        let polls = std::sync::Arc::new(AtomicU32::new(0));
        let spawns = std::sync::Arc::new(AtomicU32::new(0));
        let (_sender, receiver) = mpsc::channel::<()>();
        let policy = Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(200),
            poll: Duration::from_millis(20),
        };
        let poll_counter = polls.clone();
        let spawn_counter = spawns.clone();
        // The restart's child counts into the same counter as the first one, so the
        // measurement is the whole loop's, not one child's.
        let inside_counter = polls.clone();
        let started = Instant::now();
        let outcome = run(
            Box::new(Counted {
                inner: SpawnedChild::new(first),
                polls: poll_counter,
            }),
            &policy,
            Box::new(move || {
                spawn_counter.fetch_add(1, Ordering::SeqCst);
                let child = real_child("/bin/echo").expect("echo starts");
                Ok(Box::new(Counted {
                    inner: SpawnedChild::new(child),
                    polls: inside_counter.clone(),
                }) as Box<dyn ModelProcess>)
            }),
            &receiver,
            &mut |_| {},
            &Liveness::new(),
        );
        let elapsed = started.elapsed();
        assert_eq!(outcome, Outcome::GaveUp { restarts: 1 });
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "one restart, not a flock");
        assert!(
            elapsed >= Duration::from_millis(200),
            "the backoff was skipped: {:?}",
            elapsed
        );
        assert!(
            polls.load(Ordering::SeqCst) <= 5,
            "{} status checks for a two-death loop: the supervisor was spinning",
            polls.load(Ordering::SeqCst)
        );
    }
}
