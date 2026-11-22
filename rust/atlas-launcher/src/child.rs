//! The one way this process spawns, reads and stops a child.
//!
//! Every child the launcher starts — `llama-server`, then the window
//! (`atlas-desktop`) — is spawned through this module, so three rules hold for all
//! of them and are written once:
//!
//! 1. **The environment is an allowlist.** `env_clear` plus
//!    [`atlas_config::spawn_env`]: `PATH`, `HOME`, `TMPDIR`, `LANG` and nothing
//!    else. A token in the user's shell is not visible to a child of ours.
//! 2. **No shell.** argv is a list of arguments, never a string a shell parses.
//! 3. **A child is reaped, and only ours is signalled.** [`terminate`] asks
//!    politely (`SIGTERM` through `kill(1)`) for as long as the grace allows, then
//!    uses `Child::kill`. It never names a pid it did not get from its own
//!    `Child` handle.
//!
//! It decides nothing about *which* program to run or *when* to restart one; that
//! is [`crate::llama`] and [`crate::supervise`].

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Run a program with a deadline and return its stdout as text.
///
/// Used for the two local `--version`/`--help` questions the launcher asks. A
/// program that never answers is killed at the deadline and what it wrote so far
/// is returned, so a help probe can never hang the boot.
pub fn capture(program: &Path, args: &[String], timeout: Duration) -> Result<String, String> {
    if !program.is_file() {
        return Err(format!("{} does not exist", program.display()));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (name, value) in atlas_config::spawn_env(&[]) {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot run {}: {}", program.display(), error))?;
    // Read stdout on a thread so the pipe can never fill up while we poll the
    // child; the thread ends when the pipe closes, which includes the kill below.
    let mut pipe = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            // Not reachable through `Stdio::piped()` above, but the branch is here,
            // so the child it abandons has to be dealt with: this used to return
            // with the process still running and never waited for — a child of ours
            // outliving the probe that started it.
            abandon(&mut child);
            return Err("the child has no stdout".to_string());
        }
    };
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    // A help text is text; anything else would be a broken binary
                    // and is not worth a byte-exact answer.
                    out.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    if out.len() > 1_000_000 {
                        break;
                    }
                }
            }
        }
        out
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    abandon(&mut child);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                // `kill` without `wait` leaves the entry in the process table as a
                // zombie for as long as the launcher runs, and a zombie still holds
                // the pid. This branch could not reap with `try_wait` — that is the
                // call that just failed — so it waits.
                abandon(&mut child);
                return Err(format!("cannot read the child's status: {}", error));
            }
        }
    }
    match reader.join() {
        Ok(text) => Ok(text),
        Err(_) => Err("the output reader stopped".to_string()),
    }
}

/// Ask a child to stop, then make it.
///
/// `SIGTERM` first where a `kill(1)` exists, because a model server given a moment
/// to close its sockets leaves nothing behind; `SIGKILL` after the grace, because
/// the rule is that no child outlives ATLAS. Returns the sentence to print.
pub fn terminate(child: &mut Child, grace: Duration) -> String {
    let pid = child.id();
    if let Ok(Some(_)) = child.try_wait() {
        return format!("pid {} had already exited", pid);
    }
    let polite = politely(pid);
    let deadline = Instant::now() + wait_after(polite, grace);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return if polite {
                    format!("pid {} stopped after SIGTERM", pid)
                } else {
                    format!("pid {} stopped", pid)
                }
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break,
        }
    }
    abandon(child);
    format!("pid {} did not stop within {}s and was killed", pid, grace.as_secs())
}

/// How long to wait for a child *after* asking it to stop.
///
/// A wait with no request in it is not a grace period, it is a pause. On Windows
/// [`politely`] has nothing to run — there is no `kill(1)`, and asking a window to close
/// needs a window message this crate does not send — so the old code sat out the whole
/// grace for a process that had not been asked anything, and only then killed it. That
/// is a frozen console at exactly the moment the person asked ATLAS to stop, on the one
/// platform where the kill was always going to be immediate. No delivered request means
/// nothing to wait for.
fn wait_after(polite: bool, grace: Duration) -> Duration {
    if polite {
        grace
    } else {
        Duration::ZERO
    }
}

/// Kill a child and wait for it: the two halves are one decision.
///
/// `Child::kill` only *sends* the signal. Until somebody waits, the operating system
/// keeps the entry in the process table as a zombie, holding the pid, and the child
/// has not really been reaped — `kill -0` on it still succeeds. `capture`'s error
/// branches used to return straight after `kill()`, which is exactly that bug.
///
/// Returns whether the kill itself was accepted (a child that has already exited
/// refuses it, and is reaped here all the same).
fn abandon(child: &mut Child) -> bool {
    let killed = child.kill().is_ok();
    let _ = child.wait();
    killed
}

/// `kill -TERM <pid>` for a pid from our own [`Child`] handle.
///
/// `kill(1)` is used instead of a `libc` call so this crate keeps its promise of
/// no `unsafe` and no new dependency. It is spawned and reaped here, and it is the
/// only program this module runs with a pid argument.
fn politely(pid: u32) -> bool {
    // There is no `kill(1)` to run on Windows: whether this platform *has* one is
    // `atlas-platform`'s answer, not a second `cfg!` here.
    if atlas_platform::host::is_windows() {
        return false;
    }
    let pid_text = pid.to_string();
    for program in ["/bin/kill", "/usr/bin/kill"] {
        if !Path::new(program).is_file() {
            continue;
        }
        let spawned = Command::new(program)
            .args(["-TERM", pid_text.as_str()])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut killer) = spawned {
            let _ = killer.wait();
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    // The process-table probe is shared with the other test modules
    // (): one implementation of "is there an entry left for this
    // pid", so every reaping test is measured the same way.
    use crate::tests::{process_entry, ps_program};

    /// A real child that would run for longer than any test: `/bin/sleep`, started
    /// through argv, no shell.
    fn sleeper() -> Option<Child> {
        if !Path::new("/bin/sleep").is_file() {
            return None;
        }
        Some(
            Command::new("/bin/sleep")
                .arg("30")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("sleep starts"),
        )
    }

    #[test]
    fn a_stop_with_no_request_delivered_does_not_wait_for_one() {
        // The Windows path, as arithmetic, so it is measured on every platform: a
        // terminate that could not ask the child to stop must not sit out the grace
        // before killing it, because nothing is on its way.
        let grace = Duration::from_secs(5);
        assert_eq!(wait_after(true, grace), grace, "a SIGTERM earns its full grace");
        assert_eq!(
            wait_after(false, grace),
            Duration::ZERO,
            "no polite stop means no wait: the kill is next"
        );
    }

    #[test]
    fn capture_returns_stdout_of_a_real_child() {
        // `/bin/sh -c` is deliberately not used: a path that always exists and
        // takes an argument is enough, and `echo` is on every POSIX machine.
        let program = Path::new("/bin/echo");
        if !program.is_file() {
            return;
        }
        let text = capture(program, &["atlas".to_string()], Duration::from_secs(5)).unwrap();
        assert_eq!(text.trim(), "atlas");
    }

    #[test]
    fn capture_refuses_a_missing_program_instead_of_panicking() {
        let error = capture(
            Path::new("/nonexistent/atlas-nope"),
            &[],
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.contains("does not exist"), "{}", error);
    }

    #[test]
    fn a_child_that_exits_is_returned_by_terminate_without_a_kill() {
        let mut child = Command::new("/bin/echo")
            .arg("done")
            .stdout(Stdio::null())
            .spawn()
            .expect("echo starts");
        // Let it finish first, so this exercises the "already exited" branch.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let sentence = terminate(&mut child, Duration::from_millis(100));
        assert!(sentence.contains("already exited"), "{}", sentence);
    }

    #[test]
    fn a_child_that_would_run_forever_is_stopped() {
        // `sleep` is on every POSIX machine; it is started through argv, no shell.
        let Some(mut child) = sleeper() else {
            return;
        };
        let pid = child.id();
        let started = Instant::now();
        let sentence = terminate(&mut child, Duration::from_secs(2));
        assert!(
            sentence.contains("stopped") || sentence.contains("killed"),
            "{}",
            sentence
        );
        assert!(started.elapsed() < Duration::from_secs(6));
        // And it really is gone: a second look must not find it running.
        assert!(matches!(child.try_wait(), Ok(Some(_))));
        // "Gone" means the operating system has no entry left, not merely that a
        // signal was sent: `kill -0` succeeds on a zombie, so the state is what is
        // asked. `terminate` reaps on every path it owns.
        if ps_program().is_some() {
            assert_eq!(process_entry(pid), None, "pid {} is still in the process table", pid);
        }
    }

    #[test]
    fn a_killed_child_that_is_never_waited_for_is_a_zombie() {
        // The measurement the test above depends on, measured: `kill` alone leaves
        // the entry behind, and `ps` is what can see it. Without this, the assertion
        // above could pass for the wrong reason (a probe that never sees anything).
        let Some(mut child) = sleeper() else {
            return;
        };
        if ps_program().is_none() {
            let _ = child.wait();
            return;
        }
        let pid = child.id();
        // A live child: running, not a zombie.
        let running = process_entry(pid).expect("a running child has an entry");
        assert!(
            !running.starts_with('Z'),
            "pid {} was a zombie while it slept: {:?}",
            pid,
            running
        );
        child.kill().expect("the signal is accepted");
        // Killed, not waited for: this is the state `capture`'s error branch used to
        // leave behind for the whole life of the launcher.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = process_entry(pid);
        while Instant::now() < deadline {
            state = process_entry(pid);
            if matches!(&state, Some(text) if text.starts_with('Z')) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(&state, Some(text) if text.starts_with('Z')),
            "kill alone did not leave a zombie, so this probe cannot see one: {:?}",
            state
        );
        // And the wait that `abandon` performs is what removes the entry.
        let _ = child.wait();
        let after = Instant::now() + Duration::from_secs(5);
        while Instant::now() < after && process_entry(pid).is_some() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(process_entry(pid), None, "pid {} survived the wait", pid);
    }

    #[test]
    fn abandon_kills_and_reaps_in_one_step() {
        // The helper both error branches of `capture` and the forced stop in
        // `terminate` run. Measured before the fix: `capture`'s `Err` branch did
        // `child.kill()` and returned, leaving the entry in the table until the
        // launcher exited.
        let Some(mut child) = sleeper() else {
            return;
        };
        let pid = child.id();
        assert!(abandon(&mut child), "a sleeping child accepts the kill");
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "the child must have been waited for, not merely signalled"
        );
        if ps_program().is_some() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && process_entry(pid).is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(process_entry(pid), None, "pid {} is still in the process table", pid);
        }
    }
}
