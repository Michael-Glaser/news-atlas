//! The product: `atlas-desktop`, started as a child and waited for.
//!
//! `atlas start` used to run the web listener in this process and point the user's
//! own browser at it. The product is now one native window — the user's hard rule is
//! that no port is ever opened — so the last step of the boot is a **child process**:
//! `rust/target/release/atlas-desktop`, which draws the archive from `data/atlas.db`
//! itself, opens no listener and starts no browser.
//!
//! Three rules, and each one is why the code is here and not in `start`:
//!
//! * **One program, named.** [`binary`] looks in the workspace's own `target/release`
//!   first and on `PATH` second, through `atlas-platform` (so the `.exe` suffix is the
//!   platform's answer, not a guess). Nothing is downloaded: a missing window is a
//!   sentence with the path and the build command.
//! * **The same child discipline as the model server.** The environment is
//!   [`atlas_config::spawn_env`] and nothing else, stdin is `/dev/null`, the working
//!   directory is the project root (so a window started from a shortcut still reads
//!   this checkout), and the child is ours to wait for and to stop.
//! * **Closing the window is how ATLAS stops.** [`Window::wait`] resolves when the
//!   child exits or when a signal asks ATLAS to stop; either way the caller goes on to
//!   stop the model server, which is the only thing that must not be left running.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use atlas_config::Paths;

use crate::report::Printer;
use crate::signals::Stop;

/// The window binary's name, as `cargo build -p atlas-desktop` writes it.
pub const BINARY_NAME: &str = "atlas-desktop";

/// How often [`Window::wait`] looks at the child.
///
/// A quarter second: the wait is a person closing a window, and the loop also reads
/// the stop flag, so a Ctrl-C in the terminal is noticed within one tick. A blocking
/// `wait()` would see the signal only after the window was gone.
pub const POLL: Duration = Duration::from_millis(250);

/// Where the workspace puts its release binaries.
pub fn release_dir(paths: &Paths) -> PathBuf {
    paths
        .root
        .join("rust")
        .join("target")
        .join("release")
}

/// The window binary: this workspace's build first, then `PATH`.
///
/// In that order because a checkout is the normal case and `PATH` is the installed
/// one; both are answers to "which program draws the window", and a build that is
/// missing there is reported rather than searched for anywhere else.
pub fn binary(paths: &Paths) -> Option<PathBuf> {
    let local = atlas_platform::host::app_binary(release_dir(paths), BINARY_NAME);
    if atlas_platform::executable::is_executable(&local) {
        return Some(local);
    }
    atlas_platform::executable::on_path(BINARY_NAME)
}

/// The sentence for "this machine has no window binary", with the path and the fix.
///
/// One line, no newline: it goes through `report::write_line`, which refuses a second
/// line and every control character in a path.
pub fn missing_line(paths: &Paths) -> String {
    format!(
        "{} was not found in {} or on PATH. Build it with `cargo build --release \
         --offline --locked -p atlas-desktop` in rust/; nothing is downloaded. Without the \
         window there is nothing to show, so `atlas start` stops here",
        BINARY_NAME,
        release_dir(paths).display()
    )
}

/// A running window, with the pid every message about it quotes.
pub struct Window {
    pid: u32,
    child: Child,
}

/// How waiting for the window ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The user closed it (or it crashed): the sentence says which, with the status.
    Closed(String),
    /// A signal asked ATLAS to stop while the window was open.
    Asked,
}

impl Window {
    /// Start the child. `Err` is the sentence to print — a window that will not
    /// spawn is a failed start, because the window *is* the product.
    pub fn launch(paths: &Paths, printer: &Printer) -> Result<Window, String> {
        let program = binary(paths).ok_or_else(|| missing_line(paths))?;
        // The workspace's one spawn boundary (`atlas-platform/src/child.rs`), not a hand
        // built `Command`: it clears the environment, adds the allowlist, and on Windows
        // hides the console window this GUI-less parent would otherwise flash for every
        // child it starts. The groups the environment is *built* from stay
        // `atlas-config`'s ([`atlas_config::spawn_env`]) because that is the place that
        // says out loud which variable it refused to pass.
        let mut command =
            atlas_platform::child::command_with(&program, &atlas_config::spawn_env(&[]));
        // The window takes no argument: it is the product and reads its own root from the
        // working directory below, so nothing from a command line can aim it elsewhere.
        command.current_dir(&paths.root).stdin(Stdio::null());
        let child = command.spawn().map_err(|error| {
            format!(
                "{} will not start ({}). The window is ATLAS; check that the file is an \
                 executable for this platform and run the build again if it is not",
                program.display(),
                error
            )
        })?;
        let pid = child.id();
        printer.note(format!("the window is {} (pid {})", program.display(), pid));
        Ok(Window { pid, child })
    }

    /// The pid, for the lines that report the child.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait until the window is closed or ATLAS is asked to stop.
    ///
    /// Neither ending is a failure: one is the user closing the product, the other is
    /// the user pressing Ctrl-C. Both leave the caller to the shutdown order, which is
    /// where the model server is stopped.
    pub fn wait(&mut self, stop: &Stop) -> Ending {
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    return Ending::Closed(format!(
                        "the window closed after {} ({})",
                        crate::report::span(started.elapsed()),
                        status_word(status)
                    ))
                }
                // A child that cannot be asked (the OS keeps no status for it) is
                // treated as closed: waiting for ever would leave the model server
                // running with nothing on screen.
                Err(error) => return Ending::Closed(format!("the window is gone ({})", error)),
                Ok(None) => {}
            }
            if stop.requested() {
                return Ending::Asked;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Stop and reap the child, whether it is still running or already gone.
    ///
    /// The shutdown step. A window still open when ATLAS stops is *ours* — this process
    /// started it — so it is asked first (`SIGTERM`, or the platform's polite stop) and
    /// then reaped; `child::terminate` is the one implementation of that, shared with
    /// the model server.
    pub fn reap(&mut self) -> String {
        crate::child::terminate(&mut self.child, crate::supervise::STOP_GRACE)
    }
}

/// An exit status as a person reads it: the code, or the signal that ended it.
fn status_word(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {}", code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {}", signal);
        }
    }
    "no exit code".to_string()
}

/// Does this path name the window binary? Used by the doctor's check, which has a
/// directory rather than a project root.
pub fn present(dir: &Path) -> bool {
    atlas_platform::executable::is_executable(atlas_platform::host::app_binary(dir, BINARY_NAME))
}

#[cfg(test)]
impl Window {
    /// A window around a child this test spawned itself.
    ///
    /// [`Window::launch`] would start `atlas-desktop`, which draws a window on the
    /// machine running the tests — so the waiting and reaping are tested against a
    /// stand-in child (`/bin/sleep`, or this test binary) and the launch path is covered
    /// by the sentence it returns when the binary is missing.
    pub fn from_child(child: Child) -> Window {
        let pid = child.id();
        Window { pid, child }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The tests start `/bin/echo` and `/bin/sleep` themselves, to measure what a real
    // child's exit status looks like. The shipped launcher no longer builds a `Command`
    // here: its one child goes through `atlas_platform::child`.
    use std::process::Command;

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-window-{}-{}-{}",
            tag,
            std::process::id(),
            atlas_store::now_iso().replace([':', '.'], "-")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rust").join("target").join("release"))
            .expect("the release directory");
        dir
    }

    #[test]
    fn the_window_is_looked_for_in_this_workspace_first() {
        let dir = root("lookup");
        let paths = Paths::at(&dir);
        let local = atlas_platform::host::app_binary(release_dir(&paths), BINARY_NAME);
        assert!(local.starts_with(&dir), "{:?}", local);
        assert!(
            local.to_string_lossy().contains("target/release"),
            "the workspace build is {:?}",
            local
        );
        assert_eq!(release_dir(&paths), dir.join("rust/target/release"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_window_is_a_sentence_with_the_path_the_fix_and_no_download() {
        let dir = root("missing");
        let paths = Paths::at(&dir);
        let sentence = missing_line(&paths);
        assert!(sentence.contains(BINARY_NAME), "{}", sentence);
        assert!(
            sentence.contains(&release_dir(&paths).display().to_string()),
            "the sentence must name where it looked: {}",
            sentence
        );
        assert!(sentence.contains("cargo build"), "{}", sentence);
        assert!(sentence.contains("nothing is downloaded"), "{}", sentence);
        assert!(
            !sentence.contains('\n') && !sentence.contains('\r'),
            "one line: {:?}",
            sentence
        );
        // And a root with no build has no binary: the PATH fallback is the only way
        // one can be found, and this test does not depend on what is installed.
        if atlas_platform::executable::on_path(BINARY_NAME).is_none() {
            assert_eq!(binary(&paths), None);
            assert!(!present(&release_dir(&paths)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_exit_status_is_quoted_as_a_code_and_never_as_an_invented_success() {
        let ok = Command::new("/bin/echo")
            .arg("x")
            .stdout(Stdio::null())
            .status();
        if let Ok(status) = ok {
            assert!(status_word(status).contains("exit code 0"), "{}", status_word(status));
        }
    }

    /// A child that exits at once, or one that would run for a minute.
    fn child(seconds: &str) -> Option<Child> {
        Command::new("/bin/sleep")
            .arg(seconds)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    #[test]
    fn waiting_ends_when_the_window_is_closed_and_says_so_with_its_status() {
        // The product's normal ending: the user closes the window. `wait` must return,
        // must not hang on the stop flag, and must say how it ended.
        let Some(child) = child("0") else { return };
        let mut window = Window::from_child(child);
        let pid = window.pid();
        assert!(pid > 0, "a launched window has a pid to report");
        match window.wait(&Stop::idle()) {
            Ending::Closed(sentence) => {
                assert!(sentence.contains("the window closed"), "{}", sentence);
                assert!(sentence.contains("exit code 0"), "{}", sentence);
            }
            other => panic!("a closed window is not {:?}", other),
        }
    }

    #[test]
    fn a_stop_while_the_window_is_open_ends_the_wait_without_killing_it_here() {
        // Ctrl-C: the wait returns `Asked` at once, and the *shutdown step* is what
        // stops the child — that separation is what keeps a signal handler from killing
        // the window in the middle of a frame.
        let Some(child) = child("30") else { return };
        let mut window = Window::from_child(child);
        let started = Instant::now();
        let ending = window.wait(&Stop::fired());
        assert_eq!(ending, Ending::Asked);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait did not notice the stop: {:?}",
            started.elapsed()
        );
        // And the child is still there for the shutdown step to reap.
        assert!(window.child.try_wait().expect("ask the child").is_none());
        let sentence = window.reap();
        assert!(sentence.contains("pid"), "{}", sentence);
        assert!(window.child.try_wait().expect("reap").is_some());
    }

    #[test]
    fn reaping_a_window_that_already_closed_is_not_a_failure() {
        let Some(child) = child("0") else { return };
        let mut window = Window::from_child(child);
        let _ = window.wait(&Stop::idle());
        // The shutdown step runs after the wait on the normal path, so it must be a
        // no-op on a child that is gone rather than an error.
        let sentence = window.reap();
        assert!(
            sentence.contains("had already exited"),
            "a window that closed is reaped, not killed again: {}",
            sentence
        );
        assert!(window.child.try_wait().expect("reap").is_some());
    }
}
