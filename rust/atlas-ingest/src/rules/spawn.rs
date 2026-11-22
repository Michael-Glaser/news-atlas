//! One engine as a child process: payload on stdin, one JSON object on stdout.
//!
//! Decides: how the child is started (the curated environment from
//! `atlas-platform`, stdin/stdout piped, stderr dropped), how it is waited for
//! under one deadline, and that a timeout kills a process **this crate** started.
//!
//! Does not decide: what to ask, or what the answer means — [`super::NativeRules`]
//! does both. **No shipped question travels this way**: all five engines expose
//! `run(op, raw)`, the function their binary runs, and are asked in-process
//! ([`super::inproc`]). This path is kept for the parity witness
//! (`NativeRules::spawn_only`), which asks the binaries the same questions so a
//! drift between library and binary fails a test instead of reaching an article,
//! and for a host that would rather pay 2–4 ms than trust in-process state.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::rules::answer::Spoken;
use crate::rules::RulesError;

/// The file of one engine in `dir`, with this platform's suffix.
///
/// `.exe` on Windows: `atlas-platform` owns the suffix, so a build for one
/// platform cannot spell another platform's binary names.
pub(crate) fn bin_path(dir: &Path, engine: &str) -> PathBuf {
    atlas_platform::host::app_binary(dir, engine)
}

/// A real executable (not a directory, not a dangling symlink) — the execute bit
/// on POSIX, an executable extension on Windows, decided by `atlas-platform`.
pub(crate) fn is_executable(path: &Path) -> bool {
    atlas_platform::executable::is_executable(path)
}

/// Ask one engine one question as a process.
///
/// The child is spawned *here*, so killing it on a timeout is killing a process
/// this crate started — never somebody else's. Output is read on its own thread:
/// an `extract` of a 600 kB page can write more than a pipe buffer holds, and a
/// single-threaded read-then-wait would deadlock at exactly the wrong size.
pub(crate) fn ask(
    path: &Path,
    engine: &str,
    op: &str,
    payload: &str,
    timeout: Duration,
) -> Result<Spoken, RulesError> {
    if !is_executable(path) {
        return Err(RulesError::missing(engine, path));
    }
    // The engine gets the curated environment and nothing else: an inherited
    // `LD_PRELOAD`/`DYLD_*` or a shell token is not part of what a text engine
    // needs, and on Windows this also keeps the console window closed.
    let mut child = atlas_platform::child::command(path)
        .arg(op)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            RulesError::new(format!(
                "{} {} could not start: {}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("engine"),
                op,
                error
            ))
        })?;

    let bytes = payload.as_bytes().to_vec();
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RulesError::new(format!("{} {} did not take a stdin pipe", engine, op)));
        }
    };
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
        let _ = stdin.flush();
        drop(stdin);
    });
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RulesError::new(format!("{} {} did not take a stdout pipe", engine, op)));
        }
    };
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::with_capacity(4096);
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = writer.join();
                    let _ = reader.join();
                    return Err(RulesError::new(format!(
                        "{} {} did not answer within {} s",
                        engine,
                        op,
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = reader.join();
                return Err(RulesError::new(format!(
                    "{} {} could not be waited for: {}",
                    engine, op, error
                )));
            }
        }
    };
    let _ = writer.join();
    let output = reader.join().unwrap_or_default();
    Ok(Spoken {
        text: String::from_utf8_lossy(&output).trim().to_string(),
        ok: status.success(),
    })
}
