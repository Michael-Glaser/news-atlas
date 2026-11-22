//! Every way this process is told to stop, as one future.
//!
//! The launcher is a foreground process, so on POSIX the two ways it is told to stop are
//! the terminal's interrupt (`SIGINT`) and the operating system's terminate (`SIGTERM`).
//! Both mean the same thing here: stop the children in [`crate::shutdown`]'s order and
//! exit 0. Windows has no signals; it has console events, and there are three of them
//! that mean "stop" — Ctrl-C, Ctrl-Break and the console window closed with its X. All
//! three are handled, because the last one is the ordinary way a person stops a
//! Windows program, and a stop request nobody hears is a model server left running.
//!
//! `SIGHUP` is deliberately not handled: closing a terminal is not a request to
//! kill a supervisor, and the app is meant to be started from one. `SIGKILL`
//! cannot be handled, which is exactly why no state is left half-written — the
//! database is SQLite in WAL mode and every file is written by an atomic call.

/// The stop request, installed once and watched from then on.
///
/// A `SIGTERM` that arrives *during* the boot — while the model is loading, say —
/// must not kill this process with the default disposition and leave a child
/// behind. So the handler is installed before any child exists, and every wait in
/// the boot (`health::wait`'s abort, `supervise`'s restarts) watches this instead of
/// the signal itself.
#[derive(Clone)]
pub struct Stop {
    rx: tokio::sync::watch::Receiver<bool>,
}

impl Stop {
    /// Has a stop been requested? Safe to call from any thread, any time.
    pub fn requested(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolve now if a stop was already requested, otherwise on the signal.
    pub async fn wait(mut self) {
        loop {
            if self.requested() {
                return;
            }
            if self.rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Install the handler on this runtime. Call it once, before anything is spawned.
pub fn install(handle: &tokio::runtime::Handle) -> Stop {
    let (tx, rx) = tokio::sync::watch::channel(false);
    handle.spawn(async move {
        stop_requested().await;
        let _ = tx.send(true);
    });
    Stop { rx }
}

/// A [`Stop`] in the two states a boot can be in, without a signal.
///
/// A test must never raise `SIGINT`/`SIGTERM` at the test runner, so the flag is built
/// directly. That used to make these constructors `#[cfg(test)]`, which meant an
/// *integration* test (a file in `tests/`, a separate crate that cannot see
/// `cfg(test)` items) had to build a runtime and call [`install`] to obtain a `Stop` it
/// could read — a real runtime and a real signal handler, to ask "what does the boot do
/// when nothing asked it to stop?". They are part of the crate's API for exactly that
/// question, in both states: [`Stop::idle`] is a boot nobody has signalled, and
/// [`Stop::fired`] is one a `SIGTERM` reached before the children were up.
///
/// Production code has no other way to set the flag: [`install`] is the only constructor
/// the boot calls, and nothing outside a test calls these two.
///
/// The doc test below is the point of dropping `#[cfg(test)]`: it compiles as a *separate*
/// crate, which is what an integration test (`tests/children.rs`) is, and a
/// `cfg(test)`-only item is invisible there.
///
/// ```
/// use atlas_launcher::signals::Stop;
///
/// // A boot nobody has signalled, and one a `SIGTERM` reached before the children were up.
/// assert!(!Stop::idle().requested());
/// assert!(Stop::fired().requested());
/// ```
impl Stop {
    /// Nothing asked ATLAS to stop: what [`install`] returns before a signal.
    pub fn idle() -> Stop {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        Stop { rx }
    }

    /// A stop that was already requested (a `SIGTERM` during the boot).
    pub fn fired() -> Stop {
        let (_tx, rx) = tokio::sync::watch::channel(true);
        Stop { rx }
    }
}

/// Resolve when the user asks ATLAS to stop.
pub async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(_) => {
                // No signal stream: fall back to the runtime's Ctrl-C handler so a
                // failure here cannot leave the app unstoppable.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(_) => {
                interrupt.recv().await;
                return;
            }
        };
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }
    // Windows tells a console program to stop three ways, and only one of them is the
    // interrupt `ctrl_c` listens for:
    //
    //   `CTRL_C_EVENT`      Ctrl-C
    //   `CTRL_BREAK_EVENT`  Ctrl-Break
    //   `CTRL_CLOSE_EVENT`  the console window closed with its X
    //
    // The last one is why this arm exists. Until it did, the X was not a stop request as
    // far as this process was concerned: Windows terminated it without running the
    // shutdown order, and — unlike POSIX — it does not kill a child when its parent
    // exits, so `llama-server` kept running with nothing on screen and the loopback port
    // still taken. The user's report of that shape was "the app is gone but chat answers
    // until I reboot".
    //
    // `windows::ctrl_close` and friends are tokio's per-event listeners (tokio 1.54.1,
    // feature `signal`, `src/signal/windows.rs`); each is a distinct type, which is why
    // the three are awaited through their own small futures below rather than through a
    // generic helper.
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};

        let mut interrupt = ctrl_c().ok();
        let mut broken = ctrl_break().ok();
        let mut closed = ctrl_close().ok();
        if interrupt.is_none() && broken.is_none() && closed.is_none() {
            // No listener at all: fall back to the one call that cannot fail to register,
            // so a failure here cannot leave the app unstoppable. It covers Ctrl-C only,
            // which is the best this platform can do without any of the three.
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
        // A listener that could not be installed waits for ever instead of being awaited
        // on: `pending` is the honest future for "this event will never arrive here".
        tokio::select! {
            _ = async {
                match interrupt.as_mut() {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
            _ = async {
                match broken.as_mut() {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
            _ = async {
                match closed.as_mut() {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    // Anything that is neither POSIX nor Windows: the runtime's own Ctrl-C handler is
    // the only portable answer, and every wait below it still works.
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn the_future_does_not_resolve_on_its_own() {
        // If it resolved immediately, every `atlas start` would shut down at once.
        let outcome = tokio::time::timeout(Duration::from_millis(150), stop_requested()).await;
        assert!(outcome.is_err(), "the signal future resolved without a signal");
    }

    #[tokio::test]
    async fn the_installed_flag_is_false_and_a_wait_can_be_abandoned() {
        let stop = install(&tokio::runtime::Handle::current());
        assert!(!stop.requested());
        // A wait that nobody aborts stays pending — that is what makes it safe to
        // install the handler before the boot starts.
        let outcome =
            tokio::time::timeout(Duration::from_millis(80), stop.clone().wait()).await;
        assert!(outcome.is_err(), "the stop wait resolved without a signal");
        assert!(!stop.requested());
    }

    #[tokio::test]
    async fn the_two_test_flags_read_as_idle_and_fired() {
        assert!(!Stop::idle().requested());
        assert!(Stop::fired().requested());
        // A fired stop makes the async wait return at once, which is what every
        // boot wait depends on.
        tokio::time::timeout(Duration::from_millis(80), Stop::fired().wait())
            .await
            .expect("a requested stop resolves at once");
    }
}
