//! Which Rust toolchain is on this machine, and how to build with it.
//!
//! `atlas doctor` runs on a machine where nothing has been built yet, so it has to
//! answer two questions without a workspace: is there a `cargo`, is there a
//! `rustc`, and what versions are they. The MCP-style guess list is the same shape
//! `atlas-web`'s `native::cargo` uses for its build hint: `CARGO` if it points at a
//! real file, then `~/.cargo/bin`, then the two places a package manager installs
//! to.
//!
//! Nothing is downloaded and no `rustup` component is installed: a missing
//! toolchain is a sentence with the manual step, never an action.

use std::path::PathBuf;

/// The command that builds the workspace, printed in a fix hint.
pub const BUILD_HINT: &str = "cargo build --release --offline";

/// What the toolchain looks like from here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Toolchain {
    /// The `cargo` path, when one was found.
    pub cargo: Option<PathBuf>,
    /// Its first version line (`cargo 1.100.0 (3acc79c0c 2026-11-05)`).
    pub cargo_version: Option<String>,
    /// The `rustc` path, when one was found.
    pub rustc: Option<PathBuf>,
    /// Its first version line.
    pub rustc_version: Option<String>,
}

impl Toolchain {
    /// Can this machine build ATLAS?
    pub fn usable(&self) -> bool {
        self.cargo.is_some() && self.rustc.is_some()
    }

    /// The line a person reads.
    pub fn text(&self) -> String {
        match (&self.cargo, &self.rustc) {
            (Some(cargo), Some(rustc)) => format!(
                "{} at {}; {} at {}",
                short_version(self.cargo_version.as_deref(), "cargo"),
                cargo.display(),
                short_version(self.rustc_version.as_deref(), "rustc"),
                rustc.display()
            ),
            (Some(cargo), None) => format!("cargo at {} but no rustc", cargo.display()),
            (None, Some(rustc)) => format!("rustc at {} but no cargo", rustc.display()),
            (None, None) => "no cargo and no rustc".to_string(),
        }
    }
}

fn short_version(line: Option<&str>, fallback: &str) -> String {
    match line {
        Some(line) => line.trim().to_string(),
        None => format!("{} (version not reported)", fallback),
    }
}

/// The directories a per-user install lands in.
///
/// `HOME` is not the only answer: on Windows it is usually unset and
/// `USERPROFILE` is the one — `atlas-platform` already answers "the caller's home",
/// so it is asked instead of read.
///
/// The two package-manager directories are POSIX answers and are only offered on a
/// POSIX system: a Windows machine has no `/usr/local/bin`, and `\\usr\\local\\bin` is
/// not its meaning either (a path without a drive letter is resolved against the
/// current drive, so pressing that guess could find an unrelated directory). Windows
/// rustup puts cargo under `%CARGO_HOME%\bin`, default `%USERPROFILE%\.cargo\bin`, and
/// the home guess already covers the default.
fn guesses() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(home) = atlas_platform::host::home_dir() {
        out.push(home.join(".cargo").join("bin"));
    }
    // A `CARGO_HOME` that is not `~/.cargo` is the case the home guess misses, and it is
    // a plain environment read, not a shell expansion.
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        let dir = PathBuf::from(cargo_home).join("bin");
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    if !atlas_platform::host::is_windows() {
        out.push(PathBuf::from("/usr/local/bin"));
        out.push(PathBuf::from("/opt/homebrew/bin"));
    }
    out
}

/// Look for one program: an explicit environment variable first, then the guess
/// list, then a `PATH` search.
///
/// The `PATH` half is `atlas-platform`'s, not a `split(':')` written here: the
/// separator is `;` on Windows, empty entries mean "the working directory" there,
/// and a name without an extension has to be tried with each entry of `PATHEXT`.
/// A hand-split list silently finds `cargo` only on the platform it was written on.
///
/// The guess list is walked with [`atlas_platform::host::app_binary`] for the same
/// reason: a bare `join("cargo")` only ever matches on POSIX, because the file on
/// Windows is `cargo.exe`, so every guess would be a miss on the one platform where
/// the guess is most needed — rustup's own directory is not on `PATH` in a console
/// started fresh after the install.
pub fn locate(env_name: &str, program: &str) -> Option<PathBuf> {
    if let Ok(value) = std::env::var(env_name) {
        let path = PathBuf::from(value.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    for dir in guesses() {
        let candidate = atlas_platform::host::app_binary(&dir, program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    atlas_platform::executable::on_path(program)
}

/// Read the toolchain: paths, then one `--version` each.
///
/// The version calls are local files being executed with `--version`; neither
/// touches the network, and both are bounded by [`crate::child::capture`] so a
/// broken wrapper cannot hang the doctor.
pub fn detect() -> Toolchain {
    let cargo = locate("CARGO", "cargo");
    let rustc = locate("RUSTC", "rustc");
    let version = |program: &Option<PathBuf>| -> Option<String> {
        let path = program.as_ref()?;
        let text = crate::child::capture(
            path,
            &["--version".to_string()],
            std::time::Duration::from_secs(10),
        )
        .ok()?;
        text.lines()
            .next()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
    };
    Toolchain {
        cargo_version: version(&cargo),
        rustc_version: version(&rustc),
        cargo,
        rustc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_has_a_toolchain_and_it_reports_itself() {
        let toolchain = detect();
        assert!(toolchain.usable(), "{}", toolchain.text());
        let text = toolchain.text();
        assert!(text.contains("cargo"), "{}", text);
        assert!(text.contains("rustc"), "{}", text);
        assert!(!text.contains("no cargo"), "{}", text);
    }

    #[test]
    fn a_missing_toolchain_is_a_sentence_not_a_panic() {
        let empty = Toolchain::default();
        assert!(!empty.usable());
        assert_eq!(empty.text(), "no cargo and no rustc");
        let half = Toolchain {
            cargo: Some(PathBuf::from("/usr/bin/cargo")),
            ..Toolchain::default()
        };
        assert!(!half.usable());
        assert!(half.text().contains("no rustc"), "{}", half.text());
    }

    #[test]
    fn an_environment_override_that_does_not_exist_is_ignored() {
        // `locate` must not hand back a path that is not a file; the fallback chain
        // answers instead, and on a machine with cargo it answers with a file.
        let found = locate("ATLAS_LAUNCHER_NO_SUCH_VAR", "cargo");
        if let Some(path) = found {
            assert!(path.is_file(), "{}", path.display());
        }
    }

    #[test]
    fn the_program_on_this_machine_is_found_and_is_one_we_may_start() {
        // The `PATH` half of `locate` is now `atlas-platform`'s answer (which
        // understands `;`, empty entries and `PATHEXT`); this pins that the two
        // still agree on the program these tests were started with.
        let cargo = locate("ATLAS_LAUNCHER_NO_SUCH_VAR", "cargo")
            .expect("these tests are run by a cargo that is on this machine");
        assert_eq!(cargo.file_name().unwrap().to_string_lossy(), "cargo");
        assert!(atlas_platform::executable::is_executable(&cargo), "{}", cargo.display());
        // A program that is nowhere is `None`, not a guess.
        assert_eq!(
            locate("ATLAS_LAUNCHER_NO_SUCH_VAR", "atlas-no-such-program"),
            None
        );
    }

    /// The guess list answers for the system it is compiled on: the rustup directory
    /// under the caller's home, and no POSIX package-manager directory on Windows.
    ///
    /// The POSIX half of this used to be unconditional, so on Windows two of the three
    /// guesses were `/usr/local/bin` and `/opt/homebrew/bin` — paths without a drive
    /// letter, which Windows resolves against the *current* drive, i.e. a guess that can
    /// land in an unrelated directory. The home guess is the one that is always right.
    #[test]
    fn the_guess_list_is_written_for_this_platform() {
        let dirs = guesses();
        assert!(!dirs.is_empty(), "there is at least one guess");
        if let Some(home) = atlas_platform::host::home_dir() {
            assert!(
                dirs.contains(&home.join(".cargo").join("bin")),
                "the rustup directory is guessed: {dirs:?}"
            );
        }
        if cfg!(windows) {
            assert!(
                !dirs.iter().any(|dir| dir.to_string_lossy().starts_with('/')),
                "no POSIX directory on Windows: {dirs:?}"
            );
        } else {
            assert!(dirs.contains(&PathBuf::from("/usr/local/bin")), "{dirs:?}");
            assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")), "{dirs:?}");
        }
        // Every guess is walked with the platform's executable suffix, so a `cargo.exe`
        // is a hit on Windows rather than a miss in a directory that does hold it.
        for dir in dirs {
            let candidate = atlas_platform::host::app_binary(&dir, "cargo");
            let name = candidate.file_name().unwrap().to_string_lossy().to_string();
            assert_eq!(name, format!("cargo{}", atlas_platform::host::exe_suffix()), "{}", dir.display());
        }
    }

    /// A name is found under the platform's suffix, and the file it names is a file.
    ///
    /// The old assertion compared the file name to `"cargo"`, which is only true on
    /// POSIX; on Windows the file is `cargo.exe` and the name check would fail on a
    /// correct answer.
    #[test]
    fn a_guessed_program_carries_the_platform_suffix() {
        let suffix = atlas_platform::host::exe_suffix();
        assert_eq!(suffix, if cfg!(windows) { ".exe" } else { "" });
        let expected = format!("cargo{suffix}");
        if let Some(guessed) = guesses()
            .into_iter()
            .map(|dir| atlas_platform::host::app_binary(&dir, "cargo"))
            .find(|candidate| candidate.is_file())
        {
            assert_eq!(guessed.file_name().unwrap().to_string_lossy(), expected);
        }
    }
}
