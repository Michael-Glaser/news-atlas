//! Is the model server's own log silenced — and what happens when it refuses?
//!
//! `llama` writes the argv (with `--log-disable` in it always) and opens
//! `data/llama.log`; this module is the one question left over: did the binary
//! *take* the flag? A conditional flag could not answer it, because the builds that
//! cannot be asked — a `--help` probe that timed out, a help text that exited oddly
//! — are exactly the builds that log a prompt. So the flag is asked for on every
//! start, and a build that refuses it (an argument parser exits in milliseconds,
//! before it opens the weights) is started **once more without it**, with the result
//! marked [`Started::silenced`]` == false` so `crate::start` can say out loud that
//! the model's own log is not silenced.
//!
//! The trade, stated instead of hidden: a valid build that dies inside
//! [`START_GRACE`] for some other reason gets one extra spawn before the supervisor
//! takes over. An exit code cannot be classified as "unknown argument" — llama.cpp
//! answers `1` for every argument error — and reading the log text for a phrase
//! would fail silently on the next wording.
//!
//! It does not: build the argv ([`crate::llama::invocation`]), open the log
//! ([`crate::llama::log_sink`]), or watch the child ([`crate::supervise`]).

use std::path::Path;

use crate::llama::{spawn, Invocation, LOG_DISABLE};
use crate::supervise::{ModelProcess, SpawnedChild};

/// How long a fresh model server has to survive before the launcher believes its
/// argv was accepted.
///
/// An argument parser refuses in milliseconds, before it opens the weights; a
/// model that loads takes minutes. One second is the widest gap between the two
/// that a boot can pay for on every start.
pub const START_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// A model server that got past its own argument parser.
pub struct Started {
    /// The running child, to be watched by [`crate::supervise`].
    pub child: SpawnedChild,
    /// The argv it is *actually* running with: the caller's, or the same one
    /// without [`LOG_DISABLE`] when this build refused the flag. A supervisor
    /// restart uses this one, or the restart would ask for the flag again and die
    /// the same way.
    pub invocation: Invocation,
    /// Is the model server silenced? `false` means this build does not know
    /// `--log-disable`, so llama.cpp's own log is in `data/llama.log` unsilenced —
    /// the caller says so out loud ([`crate::start`] does).
    pub silenced: bool,
}

/// Start the model server, and make [`LOG_DISABLE`] stick or admit that it did not.
///
/// `--log-disable` is passed unconditionally, because the alternative — asking the
/// `--help` probe first — is silence for exactly the builds that cannot be asked
/// (a probe that times out, or a build whose help exits oddly), and those are the
/// ones that write prompt text. Asking always needs an answer for the build that
/// does not know the flag, and the answer is the one this function gives: the
/// argument parser refuses it in milliseconds (before the weights are opened, so
/// well inside [`START_GRACE`]), and the child is started **once more** without it.
/// The result carries which argv is running, so every later restart keeps it.
///
/// Two trades, stated instead of hidden:
///
/// * a *valid* build that dies within [`START_GRACE`] for some other reason also
///   gets one extra spawn before the supervisor takes over — an exit code cannot be
///   classified as "unknown argument" (llama.cpp answers `1` for every argument
///   error), and guessing from the log text would fail silently on the next wording;
/// * a build without the flag runs unsilenced, and the boot says so: the model's own
///   log is not silenced, and the flag cannot make it so.
pub fn start(invocation: &Invocation, root: &Path, log: &Path) -> Result<Started, String> {
    let mut child = spawn(invocation, root, log)?;
    let died = died_within(&mut child, START_GRACE);
    let refused = died.is_some() && asks_for_silence(invocation);
    if !refused {
        // Either it is loading, or it died for a reason the flag cannot explain.
        // Both are the supervisor's business: it reports the exit and restarts once.
        return Ok(Started {
            child,
            invocation: invocation.clone(),
            silenced: asks_for_silence(invocation),
        });
    }
    let fallback = without_log_disable(invocation);
    let child = spawn(&fallback, root, log)?;
    Ok(Started {
        child,
        invocation: fallback,
        silenced: false,
    })
}

/// Did `child` exit within `grace`? `None` means it is still running (or that its
/// status cannot be read, which is the supervisor's to report).
///
/// Polled, never `wait`ed on: a stop request must not be stuck behind a model load.
fn died_within(child: &mut SpawnedChild, grace: std::time::Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        match ModelProcess::try_wait(child) {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Does this argv ask for a silenced model server?
pub fn asks_for_silence(invocation: &Invocation) -> bool {
    invocation.args.iter().any(|arg| arg == LOG_DISABLE)
}

/// The same command line with [`LOG_DISABLE`] taken out.
///
/// Exact, not a rewrite: `--log-disable` takes no value, so no other entry can be
/// that string — the only caller-supplied value in the list is the model path, and a
/// path is compared against its own text by `the_model_path_is_one_argument_whatever_
/// it_holds`.
pub fn without_log_disable(invocation: &Invocation) -> Invocation {
    let mut quieter = invocation.clone();
    quieter.args.retain(|arg| arg != LOG_DISABLE);
    quieter
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llama::invocation;
    use crate::tests::fresh_root;

    fn model() -> std::path::PathBuf {
        std::path::PathBuf::from("/opt/atlas/models/qwen.gguf")
    }

    /// A binary that refuses the flag is started again without it — and the value
    /// says so, so the boot can print it.
    ///
    /// The settings the argv is built from, at their defaults: `read_stored` on a
    /// directory with no file answers an empty map, so `build` here is pure.
    fn config() -> atlas_config::Config {
        let paths = atlas_config::Paths::at(std::env::temp_dir().join("atlas-silence-argv"));
        let stored = atlas_config::read_stored(&paths).expect("no file, no problem");
        atlas_config::Config::build(&paths, &[], &stored)
    }

    /// `/bin/false` stands in for that binary: it exits `1` immediately whatever it
    /// is passed, which is exactly what an argument parser does with an option it
    /// does not know. What is asserted is the *argv that is running*, which is the
    /// only observable difference between "asked again" and "asked once".
    #[cfg(unix)]
    #[test]
    fn a_build_that_refuses_the_flag_is_started_again_unsilenced() {
        if !Path::new("/bin/false").is_file() {
            println!("skipped: /bin/false is not here to refuse an argument");
            return;
        }
        let root = fresh_root("logrefused");
        let log = root.join("data").join("llama.log");
        let asked = invocation(&config(), Path::new("/bin/false"), &model(), 8080, "");
        assert!(asks_for_silence(&asked));
        let mut started = start(&asked, &root, &log).expect("the child starts");
        assert!(
            !started.silenced,
            "a build that exits on the flag must not be reported as silenced"
        );
        assert!(
            !asks_for_silence(&started.invocation),
            "the running argv still asks for the flag it refused: {:?}",
            started.invocation.args
        );
        let _ = ModelProcess::stop(&mut started.child);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A child that exits *cleanly* inside the grace is not a refusal: the flag
    /// stays in the argv a restart would use.
    #[cfg(unix)]
    #[test]
    fn a_quick_clean_exit_keeps_the_flag() {
        if !Path::new("/bin/true").is_file() {
            println!("skipped: /bin/true is not here");
            return;
        }
        let root = fresh_root("logclean");
        let log = root.join("data").join("llama.log");
        let asked = invocation(&config(), Path::new("/bin/true"), &model(), 8080, "");
        let mut started = start(&asked, &root, &log).expect("the child starts");
        assert!(started.silenced, "{:?}", started.invocation.args);
        assert!(asks_for_silence(&started.invocation));
        let _ = ModelProcess::try_wait(&mut started.child);
        let _ = std::fs::remove_dir_all(&root);
    }
}
