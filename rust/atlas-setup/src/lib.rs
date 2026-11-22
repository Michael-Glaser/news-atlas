//! `atlas-setup` — provisioning a fresh machine, in Rust.
//!
//! ATLAS runs entirely on this machine: a Rust workspace, a native window, a model
//! server built from the vendored llama.cpp tree, and a 6.5 GB model file. On a
//! machine where none of that exists yet, "install it" used to be a paragraph in
//! `tools/README.md`. This crate is that paragraph, executed.
//!
//! # The one thing it is not
//!
//! It is not part of the app. No binary the app runs links this crate, the window never
//! spawns it, and it performs nothing unless a person types a subcommand: `check` reads,
//! and `model`/`llama` build or fetch, each behind `--yes` and each printing the exact
//! URL or argv first. THREAT-MODEL §5's "nothing is fetched at run time" is about the
//! app; provisioning is the one place where bytes arrive from the network, and it says
//! so out loud.
//!
//! # What "check" means
//!
//! Ten rows, one per piece the window needs: the window itself
//! (`rust/target/release/atlas-desktop`), the archive it reads (`data/atlas.db`), the map
//! outlines it draws, the settings it takes from `data/config.json`, the model file at its
//! pinned size and digest, the model server program (`bin/llama-server`), the port it does
//! not open, and the script that starts it (`run.sh`). [`check`] reports each one, says in
//! plain words what to do about a missing piece and where that piece comes from, and exits
//! 1 when one is missing. Rows are measurement only: the window is checked as a file and
//! never started, and no port is probed because the window binds none.
//!
//! The web frontend is retired, and this crate no longer provisions for it: nothing here
//! installs a wasm target or `wasm-bindgen-cli`, and nothing builds `public/ui/`.
//!
//! # A model the reader chooses
//!
//! The pinned file is only the default. [`import`] copies a local `.gguf` (never moves it)
//! or downloads an https address with the same resumable transfer, refuses anything that
//! does not begin with `GGUF`, and then names the file in `data/config.json` through
//! `atlas-config`'s own save path — one validation, one clamp, one atomic 0600 write, the
//! same code the command line uses. The window is told about it in one sentence: restart
//! ATLAS to load it.
//!
//! # Why the model is verified twice
//!
//! Size and digest, both, and the digest is over the bytes on disk *as they will be
//! read* (streamed, after the rename). A truncated 6.5 GB file loads far enough to
//! answer questions badly, which is the worst failure mode this project has; a byte
//! count catches a truncated transfer, and the digest catches a complete-looking file
//! that is not the one pinned.

pub mod check;
pub mod import;
pub mod install;
pub mod license;
pub mod llama;
pub mod model;
pub mod progress;
pub mod sha;

use std::path::{Path, PathBuf};

/// Where the pieces live, resolved once from the repository root.
///
/// The root is the directory holding `rust/`, `public/`, `models/` and `bin/` — found
/// by walking up from the running binary, or given with `--root`. Everything this
/// crate reads or writes is inside it, so a wrong root is a refusal rather than a
/// file in the wrong place.
#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    /// The root the binary lives in: `<root>/rust/target/release/atlas-setup` →
    /// `<root>`. Falls back to the working directory when the binary is elsewhere
    /// (a `cargo run` from `rust/`).
    pub fn discover() -> Result<Layout, String> {
        if let Ok(current) = std::env::current_exe() {
            for ancestor in current.ancestors().skip(1) {
                if is_root(ancestor) {
                    return Ok(Layout { root: ancestor.to_path_buf() });
                }
            }
        }
        let here = std::env::current_dir().map_err(|error| format!("no working directory: {error}"))?;
        for ancestor in here.ancestors() {
            if is_root(ancestor) {
                return Ok(Layout { root: ancestor.to_path_buf() });
            }
        }
        Err("cannot find the ATLAS root (the directory holding rust/, public/, models/ and bin/) — pass --root".to_string())
    }

    /// A root given on the command line, checked before it is used.
    pub fn at(root: impl AsRef<Path>) -> Result<Layout, String> {
        let root = root.as_ref();
        if !is_root(root) {
            return Err(format!(
                "{} is not an ATLAS root: it holds no rust/Cargo.toml",
                root.display()
            ));
        }
        Ok(Layout { root: root.to_path_buf() })
    }

    pub fn rust_dir(&self) -> PathBuf {
        self.root.join("rust")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub fn public_dir(&self) -> PathBuf {
        self.root.join("public")
    }
    pub fn llama_source(&self) -> PathBuf {
        self.root.join("tools").join("llama.cpp")
    }
    /// The file the model server is started from, per platform.
    pub fn model_server(&self) -> PathBuf {
        self.bin_dir().join(if cfg!(windows) { "llama-server.exe" } else { "llama-server" })
    }
}

fn is_root(dir: &Path) -> bool {
    dir.join("rust").join("Cargo.toml").is_file()
}

/// One line of a report: a piece, its state, and what to do when it is not there.
///
/// `fix` is owned, not a `&'static str`: an honest instruction sometimes has to name the
/// address a file comes from, or the file name the manifest pins, and those are read at
/// run time. A row that cannot say where the missing file comes from is not an
/// instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub piece: &'static str,
    pub ok: bool,
    /// True for a piece that is *not* there and does not stop the window: the row says what
    /// happens without it, and `all_ok` stays true so `check` exits 0. A warning is a fact
    /// about this machine, not a refusal to run.
    pub warn: bool,
    pub detail: String,
    pub fix: Option<String>,
}

impl Line {
    pub fn ok(piece: &'static str, detail: impl Into<String>) -> Line {
        Line { piece, ok: true, warn: false, detail: detail.into(), fix: None }
    }
    /// A piece that is missing and does not matter: the app does something else instead.
    pub fn warn(piece: &'static str, detail: impl Into<String>) -> Line {
        Line { piece, ok: true, warn: true, detail: detail.into(), fix: None }
    }
    pub fn missing(
        piece: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Line {
        Line { piece, ok: false, warn: false, detail: detail.into(), fix: Some(fix.into()) }
    }
    pub fn text(&self) -> String {
        match (&self.ok, &self.warn, &self.fix) {
            (true, true, _) => format!("  {:20} warn {}", self.piece, self.detail),
            (true, false, _) => format!("  {:20} ok   {}", self.piece, self.detail),
            (false, _, Some(fix)) => format!("  {:20} FAIL {}  -> {}", self.piece, self.detail, fix),
            (false, _, None) => format!("  {:20} FAIL {}", self.piece, self.detail),
        }
    }
}
