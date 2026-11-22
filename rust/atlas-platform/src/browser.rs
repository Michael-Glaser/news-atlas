//! Where a Chromium-based browser is, per OS.
//!
//! The AI reads the pages plain HTTP cannot reach through a browser this machine
//! already has; ATLAS never downloads one. Finding it is a platform question with
//! three different answers, so the answer lives here:
//!
//! * **Windows**: `%ProgramFiles%\Google\Chrome\Application\chrome.exe`,
//!   `%ProgramFiles(x86)%`, `%LOCALAPPDATA%` (a per-user install), the same four
//!   layouts under `C:\Program Files` and `C:\Program Files (x86)` when the
//!   variables are missing, and then `chrome`/`msedge`/… on `PATH` with
//!   `PATHEXT`.
//! * **macOS**: the `*.app/Contents/MacOS/*` bundles in `/Applications`.
//! * **Linux and the BSDs**: the distribution's names on `PATH`.
//!
//! Every function takes the platform and the environment **as parameters**, so
//! the Windows lookup is exercised by the tests that run on a POSIX machine —
//! under the old `#[cfg(target_os = "windows")]` fork it could only ever be
//! reviewed, never run.

use std::path::{Path, PathBuf};

use crate::executable;
use crate::host::{self, Os};
use crate::winpath;

/// The environment the lookup reads, as a value. A snapshot, so a caller (and a
/// test) decides what the machine "has" instead of inheriting whatever the
/// process happens to hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vars {
    pairs: Vec<(String, String)>,
}

impl Vars {
    /// A snapshot of this process's environment.
    pub fn from_process() -> Vars {
        Vars {
            pairs: std::env::vars_os()
                .map(|(name, value)| {
                    (
                        name.to_string_lossy().to_string(),
                        value.to_string_lossy().to_string(),
                    )
                })
                .collect(),
        }
    }

    /// A snapshot from explicit pairs — what a test uses.
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Vars {
        Vars {
            pairs: pairs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        }
    }

    /// One variable, or `None` when it is absent or empty.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    }
}

/// The layouts a browser is installed in, under each Windows base directory.
const WINDOWS_LAYOUTS: [(&str, &str); 4] = [
    (r"Google\Chrome\Application", "chrome.exe"),
    (r"Microsoft\Edge\Application", "msedge.exe"),
    (r"BraveSoftware\Brave-Browser\Application", "brave.exe"),
    (r"Chromium\Application", "chrome.exe"),
];

/// The `*.app` bundles in `/Applications`, most preferred first. Naming them on
/// every POSIX build costs nothing: a path that is not there is not executable.
const MACOS_BUNDLES: [&str; 5] = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Arc.app/Contents/MacOS/Arc",
];

/// The program names a POSIX `PATH` search tries, most preferred first.
const POSIX_NAMES: [&str; 7] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "brave-browser",
    "chrome",
];

/// The names a Windows `PATH` search tries (the extension comes from `PATHEXT`).
const WINDOWS_NAMES: [&str; 4] = ["chrome", "msedge", "brave", "chromium"];

/// Every executable this crate is willing to start, most preferred first.
///
/// No file system is touched: this is the ordered list, and
/// [`find`] filters it by "may be started".
pub fn candidates(os: Os, env: &Vars) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    match os {
        Os::Windows => {
            let mut bases: Vec<String> = Vec::new();
            for name in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
                if let Some(value) = env.var(name) {
                    bases.push(value.to_string());
                }
            }
            // The two directories Windows guarantees, for a process that was
            // started without the variables (a service, a stripped environment).
            bases.push(r"C:\Program Files".to_string());
            bases.push(r"C:\Program Files (x86)".to_string());
            out.extend(browser_paths_in(&bases));
            let pathext = env.var("PATHEXT").unwrap_or(host::PATHEXT_DEFAULT);
            let path_value = env.var("PATH").unwrap_or_default();
            for name in WINDOWS_NAMES {
                out.extend(executable::path_candidates(
                    name, os, path_value, pathext,
                ));
            }
        }
        Os::Posix => {
            out.extend(MACOS_BUNDLES.iter().map(PathBuf::from));
            let path_value = env.var("PATH").unwrap_or_default();
            for name in POSIX_NAMES {
                out.extend(executable::path_candidates(name, os, path_value, ""));
            }
        }
    }
    out
}

/// The browser executables under each base directory, in the documented
/// layouts.
///
/// The result is spelled with `\\`, because it is a Windows answer: the caller
/// this list serves runs on Windows, and a test on any machine can assert it
/// exactly (see [`executable::join`]).
pub fn browser_paths_in(bases: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for base in bases {
        for (dir, exe) in WINDOWS_LAYOUTS {
            out.push(PathBuf::from(format!("{}\\{}\\{}", base, dir, exe)));
        }
    }
    out
}

/// The browser this crate will start, or `None`.
///
/// An explicit path (the config's `browser_path`, `ATLAS_CHROME`) wins, but only
/// when it really is an executable file: a typo must not mean "no browser" for
/// the whole machine, and it must not mean "spawn this arbitrary file" either.
pub fn find(explicit: Option<&Path>, os: Os, env: &Vars) -> Option<PathBuf> {
    if let Some(path) = explicit {
        if executable::is_executable(path) {
            return Some(path.to_path_buf());
        }
    }
    executable::first_executable(&candidates(os, env))
}

/// [`find`] for this process: its own environment, this build's platform.
pub fn discover(explicit: Option<&Path>) -> Option<PathBuf> {
    find(explicit, host::os(), &Vars::from_process())
}

/// The base directories a hand-installed Playwright Chromium may sit in.
///
/// Playwright's own registry directory per machine, which is what a person who
/// ran `npx playwright install chromium` ends up with:
///
/// * Windows: `%LOCALAPPDATA%\ms-playwright`, and the same directory spelled
///   from `%USERPROFILE%` when that variable is the one the process has;
/// * Linux: `$XDG_CACHE_HOME/ms-playwright`, else `~/.cache/ms-playwright`;
/// * macOS: `~/Library/Caches/ms-playwright`.
///
/// Both POSIX spellings are named on every POSIX build, in the same spirit as
/// MACOS_BUNDLES: a root that is not there costs one failed `read_dir` in
/// `atlas-browser`, and the alternative — deciding that one POSIX is not the
/// other — is what left a macOS Chromium unfindable when this function answered
/// only the Linux path. Naming the directory is this crate's job; scanning it is
/// `atlas-browser`'s, because that reads the disk.
pub fn playwright_roots(os: Os, env: &Vars) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    match os {
        Os::Windows => {
            if let Some(local) = env.var("LOCALAPPDATA") {
                out.push(executable::join(local, "ms-playwright", Os::Windows));
            }
            if let Some(profile) = env.var("USERPROFILE") {
                out.push(executable::join(
                    profile,
                    r"AppData\Local\ms-playwright",
                    Os::Windows,
                ));
            }
        }
        Os::Posix => {
            if let Some(cache) = env.var("XDG_CACHE_HOME") {
                out.push(Path::new(cache).join("ms-playwright"));
            }
            if let Some(home) = env.var("HOME") {
                out.push(Path::new(home).join(".cache").join("ms-playwright"));
                out.push(
                    Path::new(home)
                        .join("Library")
                        .join("Caches")
                        .join("ms-playwright"),
                );
            }
        }
    }
    // One directory, named once: `%LOCALAPPDATA%` *is* `%USERPROFILE%\AppData\
    // Local`, and the two spellings of it differ only in case on Windows.
    let mut seen: Vec<String> = Vec::new();
    out.retain(|path| {
        let key = winpath::compare_key(&path.to_string_lossy(), os);
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    });
    out
}

/// The browser executable inside one `chromium-*` directory of a Playwright
/// install, in the three layouts Playwright uses.
pub fn playwright_layouts(dir: &Path) -> Vec<PathBuf> {
    vec![
        dir.join("chrome-linux").join("chrome"),
        dir.join("chrome-mac").join("Chromium.app").join("Contents").join("MacOS").join("Chromium"),
        dir.join("chrome-win").join("chrome.exe"),
    ]
}

/// The sentence a machine without a browser gets: what to install, and the two
/// ways to point ATLAS at one it already has.
pub fn no_browser_hint() -> &'static str {
    "install Chrome, Chromium or Edge, or point browser_path in data/config.json \
     (or the ATLAS_CHROME environment variable) at its executable"
}

/// True when this name looks like a Playwright directory (`chromium-1234`).
pub fn is_playwright_dir(name: &str) -> bool {
    name.starts_with("chromium-") || name.starts_with("chromium_headless_shell-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_lookup_reads_the_install_bases_from_the_environment() {
        let env = Vars::from_pairs(&[
            ("ProgramFiles", r"C:\Program Files"),
            ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
            ("PATH", r"C:\Windows\System32"),
            ("PATHEXT", ".COM;.EXE"),
        ]);
        let found = candidates(Os::Windows, &env);
        assert!(found.contains(&PathBuf::from(
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        )));
        assert!(found.contains(&PathBuf::from(
            r"C:\Users\me\AppData\Local\Microsoft\Edge\Application\msedge.exe"
        )));
        // The `PATH` half tries the extensions from PATHEXT, spelled the way the
        // machine spells them; Windows resolves either case.
        assert!(found.contains(&PathBuf::from(r"C:\Windows\System32\chrome.EXE")));
        assert!(found.contains(&PathBuf::from(r"C:\Windows\System32\chrome.COM")));
        // And the two guaranteed directories are always tried.
        assert!(found.contains(&PathBuf::from(
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"
        )));
    }

    #[test]
    fn a_posix_lookup_asks_the_path_and_the_app_bundles() {
        let env = Vars::from_pairs(&[("PATH", "/usr/bin:/snap/bin")]);
        let found = candidates(Os::Posix, &env);
        assert!(found.contains(&PathBuf::from("/usr/bin/google-chrome")));
        assert!(found.contains(&PathBuf::from("/snap/bin/chromium")));
        assert!(found.contains(&PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        )));
    }

    #[test]
    fn an_explicit_path_wins_only_when_it_can_be_started() {
        let env = Vars::from_pairs(&[("PATH", "/nonexistent")]);
        // A file that cannot be started is never the answer, and never the
        // answer somebody else gets either.
        let refused = PathBuf::from("/nonexistent/chrome");
        assert_ne!(find(Some(&refused), Os::Posix, &env), Some(refused));
        // A file that can be started wins over every candidate.
        let real = std::env::current_exe().expect("this test binary");
        assert_eq!(find(Some(&real), Os::Posix, &env), Some(real));
    }

    #[test]
    fn playwright_lives_under_the_per_user_data_directory() {
        let windows = Vars::from_pairs(&[("LOCALAPPDATA", r"C:\Users\me\AppData\Local")]);
        assert_eq!(
            playwright_roots(Os::Windows, &windows),
            vec![PathBuf::from(r"C:\Users\me\AppData\Local\ms-playwright")]
        );
        // The POSIX answer grew a second root (see
        // `playwright_is_found_under_the_cache_directory_this_os_uses`): this test
        // asserted the one spelling a Linux machine uses, which is why the macOS
        // answer could be missing for as long as it was.
        let posix = Vars::from_pairs(&[("HOME", "/home/me")]);
        assert_eq!(
            playwright_roots(Os::Posix, &posix),
            vec![
                PathBuf::from("/home/me/.cache/ms-playwright"),
                PathBuf::from("/home/me/Library/Caches/ms-playwright"),
            ]
        );
    }

    #[test]
    fn playwright_is_found_under_the_cache_directory_this_os_uses() {
        // Playwright's own registry directory is `~/Library/Caches/ms-playwright`
        // on macOS and `~/.cache/ms-playwright` (or `$XDG_CACHE_HOME`) on Linux.
        // Only the Linux spelling was named, so on macOS — the machine this app
        // ships to — a hand-installed Chromium was never a candidate and the
        // "install Chrome" hint was the only answer.
        let macos = Vars::from_pairs(&[("HOME", "/Users/me")]);
        assert!(playwright_roots(Os::Posix, &macos).contains(&PathBuf::from(
            "/Users/me/Library/Caches/ms-playwright"
        )));
        let xdg = Vars::from_pairs(&[("HOME", "/home/me"), ("XDG_CACHE_HOME", "/var/cache/me")]);
        assert_eq!(
            playwright_roots(Os::Posix, &xdg),
            vec![
                PathBuf::from("/var/cache/me/ms-playwright"),
                PathBuf::from("/home/me/.cache/ms-playwright"),
                PathBuf::from("/home/me/Library/Caches/ms-playwright"),
            ]
        );
        // A home directory with a space in it is one component, never two.
        let spaced = Vars::from_pairs(&[("HOME", "/Users/Ann Marie")]);
        assert!(playwright_roots(Os::Posix, &spaced).contains(&PathBuf::from(
            "/Users/Ann Marie/Library/Caches/ms-playwright"
        )));
    }

    #[test]
    fn one_playwright_directory_is_named_once_however_it_is_spelled() {
        // `%LOCALAPPDATA%` already *is* `%USERPROFILE%\AppData\Local`, so naming
        // both roots asked the same directory twice. On Windows the two spellings
        // differ only in case, which is the same directory there.
        let both = Vars::from_pairs(&[
            ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
            ("USERPROFILE", r"C:\Users\Me"),
        ]);
        let roots = playwright_roots(Os::Windows, &both);
        assert_eq!(roots, vec![PathBuf::from(r"C:\Users\me\AppData\Local\ms-playwright")]);
    }
}
