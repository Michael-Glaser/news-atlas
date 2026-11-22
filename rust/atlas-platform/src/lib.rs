//! atlas-platform — the one place that answers a question whose answer depends
//! on the operating system.
//!
//! Every crate that needs a platform answer asks here; after this crate landed,
//! nothing in the workspace asks `std::env::consts::OS` or `cfg!(windows)`
//! itself. A second implementation of "where is the browser" or "how do I make
//! this file private" is a bug, not a convenience (`docs/RUST-STYLE.md`).
//!
//! ## What this crate decides
//!
//! | Module | The answer |
//! | --- | --- |
//! | [`host`] | which OS this build is for, the executable suffix, the home/temp/local-data directories, the User-Agent platform token, whether paths are compared case-insensitively |
//! | [`winpath`] | the shape and the validity of Windows-shaped path text: drive letters, UNC, the `\\?\` device prefix, reserved device names, trailing dots and spaces, alternate data streams, mixed separators — understood on **every** platform, so the rules are testable where the CI runs |
//! | [`root`] | the app root: how it is found, and whether a path is inside it — canonicalised, case-insensitively on Windows, refusing `..` in both separator styles |
//! | [`location`] | where the data dir, the models dir, the model file, the config, the database, the binaries and the browser profile are |
//! | [`private`] | how a file is made private: 0600 in a 0700 directory on POSIX, the inherited ACL (an honest no-op) on Windows |
//! | [`executable`] | is this file executable, and where is the one of that name on `PATH` (`PATHEXT` included) |
//! | [`browser`] | where a Chromium-based browser is on this machine, per OS, from a given environment |
//! | [`child`] | how a child process is started: the environment allowlist, never the caller's environment |
//!
//! ## The rules this crate keeps
//!
//! * **`#![forbid(unsafe_code)]`**, no dependencies, and no `std::env::consts::OS`:
//!   the OS is a compile-time `cfg`, so a typo cannot make the binary claim the
//!   wrong platform.
//! * **The Windows answers are pure functions.** `winpath`, `root`'s text
//!   comparison, `browser::candidates` and `child::curate` take the platform as a
//!   parameter, so a POSIX machine exercises the Windows rules byte for byte. A
//!   `#[cfg(target_os = "windows")]` block that only a Windows CI could ever run
//!   was how the Windows lookup stayed wrong without anyone noticing.
//! * **Nothing is downloaded, nothing is spawned to decide, nothing is guessed.**
//!   The only I/O is a `stat`, a `canonicalize` and a `chmod` on paths the caller
//!   named.
//! * **Path text is read as bytes, never sliced by character count.** A path
//!   component may start with a character of three or four bytes (`中文`, `🙂`);
//!   the drive letter and the `\\?\` prefix are ASCII, so they are matched as
//!   bytes. A configured path may be refused and may be relative, but it may not
//!   panic the process that read it.
//! * **An answer that cannot be given says so.** Windows file privacy is the ACL
//!   inherited from the profile — this crate reports that as
//!   [`private::Privacy::Inherited`] instead of pretending a `chmod` happened.
//!
//! ## What it does not do
//!
//! It does not read `data/config.json` (that is `atlas-config`), it does not
//! decide what a URL may be (that is `atlas-guard`), and it never starts a
//! browser or a model: it says *where* they are and *how* a child is started.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod browser;
pub mod child;
pub mod executable;
pub mod host;
pub mod location;
pub mod private;
pub mod root;
pub mod winpath;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_paths;

pub use host::Os;
