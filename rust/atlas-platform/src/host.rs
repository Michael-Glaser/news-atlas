//! Which OS this build is for, and the answers that follow from it.
//!
//! The OS is decided at **compile time** (`cfg!`), never by reading
//! `std::env::consts::OS`: a runtime string is a value a typo or a forked
//! environment can change, and nothing in ATLAS wants a decision that can be
//! renamed from outside. In particular this crate never asks `consts::OS`, so a
//! grep for it across the workspace answers zero and stays zero.
//!
//! What lives here: the two spellings of the platform ([`Os`], [`Platform`] is
//! the same type under the name `atlas-config` publishes), the `.exe` suffix, the
//! directories a process needs (home, temp, local app data), the User-Agent
//! platform token, and whether a path comparison folds case.
//!
//! What does not live here: anything about *path text* (that is [`crate::winpath`]),
//! and anything about *file privacy* (that is [`crate::private`]).

use std::path::{Path, PathBuf};

/// The two platforms ATLAS has an answer for.
///
/// A parameter everywhere it is used, so a test on POSIX can ask the Windows
/// question — the only way a Windows-only rule gets tested at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Os {
    /// Linux, macOS, the BSDs: `/` separators, POSIX file modes.
    Posix,
    /// Windows: both separators, drive letters, ACLs, `PATHEXT`.
    Windows,
}

/// `atlas-config` publishes this type as `Platform`; the name is the only
/// difference, and both point at this enum.
pub type Platform = Os;

/// The platform this binary was compiled for.
pub fn os() -> Os {
    if cfg!(windows) {
        Os::Windows
    } else {
        Os::Posix
    }
}

/// True when this binary was compiled for Windows.
pub fn is_windows() -> bool {
    cfg!(windows)
}

/// The platform named by a `cfg`-style string (`"windows"`, `"linux"`,
/// `"macos"`), for a caller holding a name rather than a build. `None` for a
/// name this crate does not know, so an unknown platform is never silently
/// treated as POSIX.
pub fn from_name(name: &str) -> Option<Os> {
    match name.trim().to_ascii_lowercase().as_str() {
        "windows" | "win" | "win32" => Some(Os::Windows),
        "linux" | "macos" | "darwin" | "freebsd" | "openbsd" | "netbsd" | "posix" | "unix" => {
            Some(Os::Posix)
        }
        _ => None,
    }
}

/// `.exe` on Windows, nothing anywhere else.
pub fn exe_suffix() -> &'static str {
    if cfg!(windows) {
        ".exe"
    } else {
        ""
    }
}

/// The suffix for an explicit platform — the pure form of [`exe_suffix`].
pub fn exe_suffix_of(os: Os) -> &'static str {
    match os {
        Os::Windows => ".exe",
        Os::Posix => "",
    }
}

/// Extension of a Windows executable, the set `cmd` searches when a name has no
/// extension (the default of `PATHEXT`).
pub const PATHEXT_DEFAULT: &str = ".COM;.EXE;.BAT;.CMD";

/// `dir/name.exe` — the path of one ATLAS binary in a build directory.
pub fn app_binary(dir: impl AsRef<Path>, name: &str) -> PathBuf {
    dir.as_ref().join(format!("{}{}", name, exe_suffix()))
}

/// Do two paths compare case-insensitively on this platform?
///
/// Windows file systems fold case by default, so `C:\App` and `c:\app` are one
/// directory and a containment check that ignored case would be wrong. macOS
/// (APFS, case-insensitive by default) is deliberately **not** folded here: the
/// rule ATLAS keeps is the conservative one — on the one system where a
/// case-insensitive comparison could *widen* a containment verdict, do not use
/// it. POSIX is case-sensitive by definition.
pub fn folds_case() -> bool {
    cfg!(windows)
}

/// [`folds_case`] for an explicit platform.
pub fn folds_case_of(os: Os) -> bool {
    matches!(os, Os::Windows)
}

/// The platform token in the User-Agent: the real machine, not a fiction.
///
/// `atlas-net` builds `Mozilla/5.0 (<token>) …` from this; a publisher that
/// serves a desktop page to a desktop browser and a stub to an unknown client
/// then behaves the way it does for a person. It replaces a `match` on
/// `std::env::consts::OS` that could report a platform the binary is not.
pub fn navigator_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else if cfg!(windows) {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    }
}

/// The platform string a page reports as `navigator.platform`, for the browser
/// this app drives.
///
/// It is the same decision as [`navigator_platform`], in the other dialect: the
/// `User-Agent` header `atlas-net` sends and the `navigator.platform` a script in
/// the reader browser reads must agree, or a site sees one machine claiming to be
/// two. One function, so they cannot drift.
pub fn navigator_platform_js() -> &'static str {
    if cfg!(target_os = "macos") {
        "MacIntel"
    } else if cfg!(windows) {
        "Win32"
    } else {
        "Linux x86_64"
    }
}

/// The character that separates the entries of a `PATH`.
pub fn path_list_separator() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

/// The caller's home directory: `HOME`, then `USERPROFILE` on Windows.
pub fn home_dir() -> Option<PathBuf> {
    non_empty("HOME").or_else(|| non_empty("USERPROFILE"))
}

/// `%LOCALAPPDATA%` on Windows, `$XDG_DATA_HOME` or `~/.local/share` elsewhere —
/// where a per-user application keeps its state.
pub fn local_app_data() -> Option<PathBuf> {
    if cfg!(windows) {
        non_empty("LOCALAPPDATA")
    } else {
        non_empty("XDG_DATA_HOME").or_else(|| home_dir().map(|home| home.join(".local/share")))
    }
}

/// The temp directory a child and a scratch profile may use.
///
/// Windows: `TEMP`, then `TMP`, then `%LOCALAPPDATA%\Temp`, then
/// `C:\Windows\Temp` — a child that inherits no `TEMP` still needs one that
/// exists. POSIX: `TMPDIR`, then `/tmp`. `std::env::temp_dir` is not used
/// because it reads the process environment at a moment the caller cannot
/// control, and because a test must be able to ask the question with a chosen
/// environment.
///
/// **This directory is shared, and its last fallbacks are world-writable**
/// (`/tmp` is `1777`, `C:\Windows\Temp` likewise). It is the right answer for a
/// child's `TMPDIR` and for scratch nobody else must read, and the wrong answer
/// for anything private: the archive lives in `data/` (`0700`, see
/// [`crate::location`]), and a caller that needs a private directory *here*
/// creates one with [`crate::private::create_dir`] — `0700` from the `mkdir`,
/// with a name that cannot be adopted because the `mkdir` fails when it is taken.
/// A plain `create_dir_all` under this path is `0755` in a world-writable parent,
/// which is how a private file ends up in a directory another account can list.
pub fn temp_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(dir) = non_empty("TEMP").or_else(|| non_empty("TMP")) {
            return dir;
        }
        if let Some(dir) = non_empty("LOCALAPPDATA") {
            return dir.join("Temp");
        }
        return PathBuf::from(r"C:\Windows\Temp");
    }
    non_empty("TMPDIR").unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// A variable that exists and is not empty.
fn non_empty(name: &str) -> Option<PathBuf> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}
