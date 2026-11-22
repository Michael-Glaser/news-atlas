//! The monitoring agent: `atlas-monitor serve`, started as a child and stopped last.
//!
//! The product hole this module closes: the window cannot ingest. It writes no file,
//! spawns nothing and listens on nothing, so a click on "monitor this topic" could only
//! ever be stored; until this child existed, the archive grew only when a human ran
//! `atlas-ingest research` in a terminal. The agent is the always-on half — it claims
//! those stored requests from `data/requests.db`, runs them through the same pipeline
//! the window's Chat page talks to, and keeps the monitors' cadence while the window is
//! open or closed.
//!
//! Four rules, and each one is why the code is here and not in `start`:
//!
//! * **One program, named.** [`binary`] looks where [`crate::window`] looks — the
//!   workspace's own `target/release` first, `PATH` second, through `atlas-platform` so
//!   the `.exe` suffix is the platform's answer — and a missing agent is a sentence, not
//!   a silent start without it. Nothing is downloaded.
//! * **Spawned, never linked.** This crate holds no agent code: the child is a program
//!   on disk, so `atlas start`'s closure still contains no pipeline, no fetcher and no
//!   browser (`tests/no_listener.rs` is the test that keeps it so).
//! * **The same child discipline as the model server.** The environment is
//!   [`atlas_config::spawn_env`] and nothing else, stdin is `/dev/null`, the working
//!   directory is the project root. Its *stdout is inherited*: the agent prints one JSON
//!   line per action and this process parses none of it, so a reader of the launcher's
//!   output sees what the AI is doing in the same stream as the boot's own lines.
//! * **Stopped before the model.** [`crate::shutdown::ORDER`] is window → agent → model,
//!   because an agent that is still claiming work while the model server goes away would
//!   finish each claimed request with a connection error it could have avoided.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use atlas_config::Paths;

use crate::report::Printer;

/// The agent binary's name, as `cargo build -p atlas-monitor` writes it.
pub const BINARY_NAME: &str = "atlas-monitor";

/// The subcommand that runs the loop. `atlas-monitor` without it is the one-shot CLI
/// (`run`, `tick`, `alerts`), which is what the bridge and the tests drive.
pub const SERVE: &str = "serve";

/// How long a stop is given before the child is killed.
///
/// Longer than the model server's grace: the agent's longest action is one research
/// pass, and the loop finishes the action it is on before it exits (that is why it
/// handles the signal at all). Twenty seconds is a `save_article` and a log line, not a
/// whole pass — a pass that takes longer is killed here rather than holding the whole
/// shutdown, and the request it was holding is re-claimed after
/// `atlas_store::requests`' lease expires.
pub const STOP_GRACE: Duration = Duration::from_secs(20);

/// The agent binary: this workspace's build first, then `PATH`, like the window's.
pub fn binary(paths: &Paths) -> Option<PathBuf> {
    let local = atlas_platform::host::app_binary(crate::window::release_dir(paths), BINARY_NAME);
    if atlas_platform::executable::is_executable(&local) {
        return Some(local);
    }
    atlas_platform::executable::on_path(BINARY_NAME)
}

/// The sentence for "this machine has no agent binary".
///
/// One line, no newline: it goes through `report::write_line`, which refuses a second
/// line and every control character in a path. It says what is lost, because a start
/// without an agent looks exactly like a start with one until a click does nothing.
pub fn missing_line(paths: &Paths) -> String {
    format!(
        "{} was not found in {} or on PATH. Build it with `cargo build --release --offline \
         --locked -p atlas-monitor` in rust/; nothing is downloaded. Without it nothing claims \
         the window's requests and no monitor runs on its own, so `atlas start` stops here",
        BINARY_NAME,
        crate::window::release_dir(paths).display()
    )
}

/// A running agent, with the pid every message about it quotes.
pub struct Agent {
    pid: u32,
    child: Child,
}

impl Agent {
    /// Start the child. `Err` is the sentence to print — like the window, an agent that
    /// will not spawn is a failed start, because the product promises that a click is
    /// acted on.
    pub fn launch(paths: &Paths, printer: &Printer) -> Result<Agent, String> {
        let program = binary(paths).ok_or_else(|| missing_line(paths))?;
        // Through the workspace's spawn boundary, like the window and the setup child: it
        // clears the environment, adds the allowlist and hides the console window a
        // GUI-less parent would otherwise flash on Windows. The variables themselves come
        // from `atlas-config`'s allowlist, which is the code that reports what it refused.
        let mut command =
            atlas_platform::child::command_with(&program, &atlas_config::spawn_env(&[]));
        command
            .arg(SERVE)
            .current_dir(&paths.root)
            .stdin(Stdio::null())
            // Inherited, not captured: the agent's one JSON line per action is meant to
            // be read, and the launcher is the stream a reader already has open.
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let child = command.spawn().map_err(|error| {
            format!(
                "{} will not start ({}). Nothing would claim the window's requests and no \
                 monitor would run on its own, so `atlas start` stops here",
                program.display(),
                error
            )
        })?;
        let pid = child.id();
        printer.note(format!(
            "the agent is {} {} (pid {}); it prints one JSON line per action",
            program.display(),
            SERVE,
            pid
        ));
        Ok(Agent { pid, child })
    }

    /// The pid, for the lines that report the child.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Stop and reap the child: the [`crate::shutdown::Step::Agent`] step.
    ///
    /// The polite stop is `SIGTERM`, which the agent handles by finishing the action line
    /// it is on and exiting 0 — that is why this waits instead of killing at once. An
    /// agent that had already exited on its own is reported here rather than passed over:
    /// monitoring stopped when it did, and the only other trace is its own stderr.
    pub fn stop(&mut self) -> String {
        let early = matches!(self.child.try_wait(), Ok(Some(_)));
        let sentence = crate::child::terminate(&mut self.child, STOP_GRACE);
        if early {
            return format!(
                "the agent had already exited before ATLAS stopped — nothing was monitoring \
                 since then ({})",
                sentence
            );
        }
        format!("the agent {}", sentence)
    }
}

/// Does this path name the agent binary? For the doctor's check, which has a directory.
pub fn present(dir: &Path) -> bool {
    atlas_platform::executable::is_executable(atlas_platform::host::app_binary(dir, BINARY_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    // The tests start `/bin/sleep` themselves, to measure the two endings a stop can
    // find. The shipped agent no longer builds a `Command`: it goes through
    // `atlas_platform::child` like every other child of this process.
    use std::process::Command;

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-agent-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rust").join("target").join("release")).expect("dirs");
        dir
    }

    #[test]
    fn a_machine_without_the_agent_is_one_sentence_that_names_it_and_the_build() {
        let paths = Paths::at(root("missing"));
        // `PATH` is not consulted for a name nothing has installed: the sentence has to
        // name the file, the directory and the command, because it is the whole of what
        // a user gets for the commonest reason the agent cannot start.
        if binary(&paths).is_some() {
            return; // an installed atlas-monitor on this machine: nothing to prove here
        }
        let sentence = missing_line(&paths);
        assert!(sentence.contains(BINARY_NAME), "{}", sentence);
        assert!(sentence.contains("target/release"), "{}", sentence);
        assert!(sentence.contains("cargo build --release"), "{}", sentence);
        assert!(sentence.contains("nothing is downloaded"), "{}", sentence);
        assert!(!sentence.contains('\n'), "one line: {}", sentence);
    }

    #[test]
    fn the_workspace_build_is_found_before_path_is_asked() {
        let paths = Paths::at(root("local"));
        let local = atlas_platform::host::app_binary(crate::window::release_dir(&paths), BINARY_NAME);
        std::fs::create_dir_all(local.parent().expect("parent")).expect("dir");
        std::fs::write(&local, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = std::fs::metadata(&local).expect("mode").permissions();
            mode.set_mode(0o755);
            std::fs::set_permissions(&local, mode).expect("chmod");
        }
        assert_eq!(binary(&paths), Some(local));
        assert!(present(&paths.root.join("rust").join("target").join("release")));
    }

    #[test]
    fn the_agent_is_stopped_by_a_polite_signal_and_a_child_that_was_gone_says_so() {
        // The two endings a stop can find. `/bin/sleep` stands in for the agent: a stop
        // step is about the child, not the program's name.
        let Ok(child) = Command::new("/bin/sleep")
            .arg("30")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let pid = child.id();
        let mut agent = Agent { pid, child };
        let sentence = agent.stop();
        assert!(sentence.contains("the agent"), "{}", sentence);
        assert!(sentence.contains(&format!("pid {}", pid)), "{}", sentence);

        let Ok(child) = Command::new("/bin/sleep")
            .arg("0")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let mut agent = Agent { pid: child.id(), child };
        // Let it exit first, so the "already gone" branch is the one that runs.
        std::thread::sleep(Duration::from_millis(50));
        let sentence = agent.stop();
        assert!(sentence.contains("had already exited"), "{}", sentence);
        assert!(sentence.contains("nothing was monitoring"), "{}", sentence);
    }
}
