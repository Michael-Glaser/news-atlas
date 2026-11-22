//! `stop` — `SIGTERM` and `SIGINT` as a flag the loop reads between actions.
//!
//! The launcher stops its children politely: `SIGTERM` first, a grace period, then a
//! kill. An agent that dies on the default disposition leaves the request it was holding
//! claimed until its lease expires, and the launcher's log says nothing about which one.
//! So the flag exists: the loop finishes the action it is on, prints that action's line,
//! and exits 0 — which is what makes the launcher's twenty-second grace period for `Step::Agent` a
//! grace period rather than a bet.
//!
//! Why the flag and not an async loop: the loop is synchronous by design (it claims one
//! row, runs it, sleeps), and `tokio`'s signal API is this workspace's one way to *hear*
//! a signal without `unsafe` and without `libc` — the same call the launcher makes. A
//! signal runs on its own thread and sets an `AtomicBool`; nothing else in this crate
//! becomes async for it.
//!
//! It does not: handle `SIGKILL` (nothing can), interrupt an action in flight (a research
//! pass ends where the model ends it), or decide what "stop" means — [`crate::serve`]
//! does, by not claiming another row.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A stop request, as the loop reads it.
#[derive(Clone)]
pub struct Stop {
    asked: Arc<AtomicBool>,
}

impl Stop {
    /// Has a stop been requested? Safe to call between actions and between sleeps.
    pub fn requested(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }

    /// A flag nothing has raised: the state [`watch`] is in before a signal.
    ///
    /// Part of the crate's API and not a test detail, because a caller that runs one step
    /// instead of the loop — a test, or a tool that drains the queue once — has no signal
    /// of its own and still has to hand an action a stop it will never see fire.
    pub fn quiet() -> Stop {
        Stop {
            asked: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Install the handlers and answer a flag that turns true when one fires.
///
/// Called once, before the first claim. If the handlers cannot be installed (a platform
/// without them) the flag simply never fires: the loop is then stopped by the launcher's
/// grace period running out, which is the same ending the model server has.
pub fn watch() -> Stop {
    let asked = Arc::new(AtomicBool::new(false));
    let setter = Arc::clone(&asked);
    let spawned = std::thread::Builder::new()
        .name("atlas-monitor-stop".to_string())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(asked_for());
            setter.store(true, Ordering::SeqCst);
        });
    let _ = spawned;
    Stop { asked }
}

/// Resolve when the user or the launcher asks this process to stop.
async fn asked_for() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(_) => return,
        };
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(_) => return,
        };
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
impl Stop {
    /// Nothing asked this process to stop: the state [`watch`] is in before a signal.
    pub fn idle() -> Stop {
        Stop::quiet()
    }

    /// A stop that already fired (the launcher's `SIGTERM` during an action).
    pub fn fired() -> Stop {
        Stop {
            asked: Arc::new(AtomicBool::new(true)),
        }
    }

    /// A flag a test raises by hand: the loop reads it exactly as it reads a signal, so a
    /// test can prove the ending without sending `SIGTERM` at the test runner.
    pub fn switch() -> Stop {
        Stop {
            asked: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Raise it, as a signal would.
    pub fn raise(&self) {
        self.asked.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_starts_quiet_and_a_fired_one_is_what_the_loop_reads() {
        assert!(!Stop::idle().requested());
        assert!(Stop::fired().requested());
    }
}
