//! Where the window finds its data files.
//!
//! The project root is discovered from the executable, exactly like the launcher does:
//! `…/rust/target/release/atlas-desktop` walks up to the checkout, so a window started
//! from anywhere in the tree reads the same archive and the same outlines.
//!
//! Three reads live here, and each is one file, named, with no search: the archive
//! ([`archive`]), the vendored country outlines ([`geojson`]), and the one setting the
//! window takes from [`config`] — the model server's port ([`model_port`]).
//!
//! The fourth item is not a read: [`label`] turns any of those paths into the text the
//! window is allowed to show. It lives here because two modules name a file — the status
//! row and [`crate::data`]'s own sentences — and a name spelled in two places is two
//! names.
//!
//! What the window *needs* and cannot make is the last part: the model file
//! ([`model`]) and the server binary ([`server`]). Both are absent on a machine that has
//! never been set up, both are things only `atlas-setup` puts there, and both must be
//! answerable as a sentence that names the file and the command — the window starts no
//! download, so "the model is missing" without a name and a fix is a dead end. They are
//! values ([`Piece`]) rather than calls at the drawing site for the same reason the
//! other reads are functions: a sentence about a file is decided once, in one place.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The repository root: the nearest ancestor holding `rust/Cargo.toml`.
pub fn root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut here: Option<&Path> = Some(exe.as_path());
        while let Some(path) = here {
            if path.join("rust").join("Cargo.toml").is_file() {
                return path.to_path_buf();
            }
            here = path.parent();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn archive() -> PathBuf {
    root().join("data").join("atlas.db")
}

/// A file named the way a reader can act on it: `data/atlas.db`, never the absolute path
/// it happens to sit at.
///
/// A path on screen is a home directory on every machine this app is installed on, and a
/// home directory tells a passer-by who the reader is; it is also unbounded, and the row
/// that shows it is one line. So the label is built from path components rather than from
/// `Path::display`: an absolute prefix, a `..` or another platform's separator cannot walk
/// in, and a file outside the checkout is named by its own name.
///
/// It sits beside [`root`] because the file is named in two places — the status row and
/// the sentences [`crate::data::Archive::open`] returns — and two spellings of one path is
/// one of them leaking.
pub fn label(path: &Path) -> String {
    // `Component::Normal` only: a `RootDir`, a `Prefix` or a `ParentDir` is exactly the
    // part of a path that must not be printed, and dropping them is cheaper to read than
    // a chain of checks that they are absent.
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    if let Ok(relative) = path.strip_prefix(root()) {
        for part in relative.components() {
            if let Component::Normal(name) = part {
                parts.push(name.to_string_lossy().to_string());
            }
        }
    }
    if parts.is_empty() {
        return path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    parts.join("/")
}

/// The vendored country outlines (Natural Earth 1:50m, public domain).
pub fn geojson() -> PathBuf {
    let root = root();
    for name in ["ne_50m_countries.geojson", "ne_50m_countries.geo.json", "countries.geo.json"] {
        let path = root.join("public").join("vendor").join(name);
        if path.is_file() {
            return path;
        }
    }
    root.join("public").join("vendor").join("ne_50m_countries.geojson")
}

/// The reader's configuration file: `<root>/data/config.json`, beside the archive.
pub fn config() -> PathBuf {
    root().join("data").join("config.json")
}

/// The key in [`config`] that names the port the model server is started on.
///
/// Structural, not a word list: it is the one field of the settings file this window
/// reads, and [`model_port`] is the only reader, so a second key is a second decision.
const PORT_KEY: &str = "llama_port";

/// The largest `config.json` that is a config: three keys are ~60 bytes, and a cap is
/// what stops a runaway file in the user's data directory from becoming this window's
/// memory problem (the same reason [`crate::http`] caps a reply at 4 MB).
const MAX_CONFIG: u64 = 64 * 1024;

/// The port the model server listens on, as `data/config.json` names it — or `None`.
///
/// # Why this is a read and not the address
///
/// The window must not carry the model's address as a constant: the server is started
/// on the port the settings file names, and a constant that disagrees with it is a Chat
/// page that cannot answer out of the box. What this returns is a number, and the
/// caller writes `http://127.0.0.1:{port}` — so the **port** comes from a file and the
/// **host** does not.
///
/// That split is the security decision. `data/config.json` lives in a `0700` directory
/// and is `0600`, which means the reader and anything else running as the reader may
/// rewrite it. What a rewritten file can do here is choose *which port on this machine*
/// is dialled: it can make the window talk to a different local service, and it can make
/// it fail to talk to the model. It cannot make the window talk to another machine,
/// because no host, no scheme and no url can survive this function — the answer is a
/// `u16`, and the literal in [`crate::http::endpoint`] is the only host there is. A
/// config that could carry an address would be a redirect; this one carries a port.
///
/// # The five refusals
///
/// `None` — never a guess, never 0 — for a file that is missing or unreadable, a file
/// that is not JSON, a missing (or differently spelled) key, a value that is not an
/// integer in `1..=65535`, and a file larger than [`MAX_CONFIG`]. A caller that gets
/// `None` keeps its own default and can say so; a caller handed a guessed port would
/// dial an address nobody chose. Port 0 is refused here as it is in `http::endpoint`:
/// zero means "any port", which is not an address.
///
/// Nothing is written, no socket is opened, and no other file is touched — the archive
/// is not consulted, so a broken or locked `atlas.db` cannot change which port is
/// dialled (and a config cannot point the read at the archive).
pub fn model_port() -> Option<u16> {
    model_port_from(&config())
}

/// [`model_port`] over a named file: the same decision, with the path as an argument.
///
/// The seam exists because [`root`] takes no argument (it reads the executable), so a
/// test could otherwise only exercise this by re-executing the test binary inside a
/// temporary tree — the trick `tests/assets.rs` has to use for the vendor order. A
/// fixture is a better test of a parser than a copy of the parser, so the decision
/// takes its file as a value and [`model_port`] is the three lines that name it.
pub fn model_port_from(config: &Path) -> Option<u16> {
    // Size before bytes: an unreadable or far-too-large file is answered without
    // reading it, so a name that points at something enormous costs a metadata call.
    if std::fs::metadata(config).ok()?.len() > MAX_CONFIG {
        return None;
    }
    let text = std::fs::read_to_string(config).ok()?;
    let settings: Value = serde_json::from_str(&text).ok()?;
    let named = settings.get(PORT_KEY)?.as_u64()?;
    // `as_u64` is the whole "is it a number" rule: a string, a float (`8081.0`), a
    // boolean, null, an array and a negative all fail it, so none of them is rounded,
    // trimmed or coerced into a port.
    let port = u16::try_from(named).ok()?;
    // 0 is "any port" — not an address, and not something to fall back *to*.
    (port != 0).then_some(port)
}

// ---------------------------------------------------------------------------
// what the window needs and cannot make: the model file, the server binary
// ---------------------------------------------------------------------------

/// The directory the model files go in: `<root>/models`.
pub fn models_dir() -> PathBuf {
    root().join("models")
}

/// The manifest `atlas-setup model` writes: the file name this install runs, and the
/// size and digest it must have.
fn pin(models: &Path) -> PathBuf {
    models.join("model.json")
}

/// The command that provisions the pinned model file, as `atlas-setup` prints it.
const MODEL_FIX: &str = "atlas-setup model --yes";

/// The command that provisions the server binary, as `atlas-setup` prints it.
const SERVER_FIX: &str = "atlas-setup llama --yes";

/// The name `atlas-setup llama` installs the server under, inside `bin/` — spelled the way
/// that crate spells it, extension and all, because a row that looks for `llama-server` on
/// a platform that stores `llama-server.exe` reports a missing server that is right there.
fn server_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// The extension a model file has. Structural, not a name list: `.part` is what a
/// half-finished transfer is called, so requiring the extension is also what keeps a
/// partial download from being reported as the model.
pub const MODEL_EXTENSION: &str = "gguf";

/// One file the window needs, as a value: what it is, where it belongs, whether it is
/// there, and what puts it there.
///
/// It is a value rather than three calls at the drawing site because a machine that has
/// never been set up has none of these, and the row that says so must answer for each
/// without opening anything: [`present`](Piece::present) is one metadata call, and
/// [`missing`](Piece::missing) is the sentence.
#[derive(Clone, Debug)]
pub struct Piece {
    /// The words the row uses for it, sentence case: `the model file`.
    pub what: &'static str,
    /// Where it belongs, as the window would open it.
    pub path: PathBuf,
    /// The size on disk — `None` when there is no file, so "absent" and "empty" stay two
    /// answers and neither is a guess.
    pub bytes: Option<u64>,
    /// The size something *else* pinned it to (the model manifest), so a row about an
    /// absent 6.5 GB file can say how big it will be before it exists.
    pub pinned: Option<u64>,
    /// The command that provisions it. A sentence that names a missing file and not the
    /// command is a sentence a reader can do nothing with.
    pub fix: &'static str,
}

impl Piece {
    /// There, and not an empty file: a 0-byte `llama-server` is a transfer that stopped
    /// halfway, not a program, and a row that called it "there" would be the one lie this
    /// file exists to prevent.
    pub fn present(&self) -> bool {
        matches!(self.bytes, Some(size) if size > 0)
    }

    /// The name the row shows: `bin/llama-server`, never the path it sits at ([`label`]).
    pub fn label(&self) -> String {
        label(&self.path)
    }

    /// The row's words when this piece is not usable — `None` when it is.
    pub fn missing(&self) -> Option<String> {
        if self.present() {
            return None;
        }
        let named = format!("{} {} ", self.what, self.label());
        Some(match self.bytes {
            // A file of zero bytes: it is there, it is not what it should be, and saying
            // "missing" would send the reader looking for a file they can already see.
            Some(_) => format!("{named}is empty — {} replaces it", self.fix),
            None => match self.pinned {
                Some(size) => format!(
                    "{named}is missing ({} to fetch) — {} fetches it",
                    size_of(size),
                    self.fix
                ),
                None => format!("{named}is missing — {} puts it there", self.fix),
            },
        })
    }
}

/// Which model file the app loads, as far as the window can tell without loading one.
#[derive(Clone, Debug)]
pub enum Model {
    /// A file is named — by the manifest, or by being the newest `.gguf`.
    Named(Piece),
    /// Nothing names one: no readable manifest and no `.gguf` in `models/`. The row can
    /// name the directory and the command, and nothing else.
    Unnamed { directory: PathBuf },
}

impl Model {
    /// The piece, when a file is named.
    pub fn piece(&self) -> Option<&Piece> {
        match self {
            Model::Named(piece) => Some(piece),
            Model::Unnamed { .. } => None,
        }
    }

    /// The file the server would load, when one is named.
    pub fn path(&self) -> Option<&Path> {
        self.piece().map(|piece| piece.path.as_path())
    }

    /// Whether that file is there and not empty.
    pub fn present(&self) -> bool {
        self.piece().is_some_and(Piece::present)
    }

    /// The row's words when there is no model file to load — `None` when there is.
    pub fn missing(&self) -> Option<String> {
        match self {
            Model::Named(piece) => piece.missing(),
            Model::Unnamed { directory } => Some(format!(
                "no model file is named: {} holds no .{MODEL_EXTENSION} and {} names none — \
                 {MODEL_FIX} fetches the pinned one",
                label(directory),
                label(&pin(directory))
            )),
        }
    }
}

/// The model file this checkout expects, as the checkout names it.
pub fn model() -> Model {
    model_in(&models_dir())
}

/// [`model`] over a named directory: the same decision, with the path as an argument.
///
/// The seam exists for the reason [`model_port_from`] has one — [`root`] reads the
/// executable and takes no argument, so a fixture is the only way to test a parser about
/// a directory that is not this checkout's.
pub fn model_in(models: &Path) -> Model {
    let named = pinned(models).or_else(|| newest_model(models).map(|path| (path, None)));
    match named {
        Some((path, pinned)) => Model::Named(Piece {
            what: "the model file",
            bytes: std::fs::metadata(&path).ok().map(|file| file.len()),
            path,
            pinned,
            fix: MODEL_FIX,
        }),
        None => Model::Unnamed {
            directory: models.to_path_buf(),
        },
    }
}

/// The server binary the Chat page needs, as this checkout names it.
pub fn server() -> Piece {
    server_in(&root().join("bin"))
}

/// [`server`] over a named directory: the same decision, with the path as an argument.
pub fn server_in(bin: &Path) -> Piece {
    let path = bin.join(server_name());
    Piece {
        what: "the model server",
        bytes: std::fs::metadata(&path).ok().map(|file| file.len()),
        path,
        pinned: None,
        fix: SERVER_FIX,
    }
}

/// The file name and the size the manifest pins, when there is a manifest that says.
///
/// Only those two fields are read. The manifest also carries a `url`, and it is not read
/// here at all: the window fetches nothing, so an address in this file is not something it
/// may act on, and a field nobody reads is a field nobody can be tricked through. A name
/// with a separator or a `..` in it would point outside `models/`, so it is refused as a
/// name (the same shapes `atlas_setup::model` refuses) and the directory decides instead.
fn pinned(models: &Path) -> Option<(PathBuf, Option<u64>)> {
    let text = std::fs::read_to_string(pin(models)).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    let name = manifest.get("name")?.as_str()?;
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    Some((models.join(name), manifest.get("bytes").and_then(Value::as_u64)))
}

/// The model file the launcher would fall back to: the newest `.gguf` by name, the rule
/// `atlas-launcher`'s model pick and `atlas_platform::location::gguf_in` already use.
///
/// It is restated here rather than called because this crate links neither: the window
/// ships four dependencies and `atlas-platform` is not one of them, and a fifth would be
/// a dependency added to print one file name. A `.part` fails the extension test, so a
/// transfer in flight is never reported as the model.
fn newest_model(models: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(models)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(MODEL_EXTENSION))
        })
        .collect();
    found.sort();
    found.pop()
}

/// A size as a reader reads it, in the same units and the same decimals as
/// `atlas_setup::check::size_text`: a reader sees "6.5 GB" in the terminal and "6.5 GB" in
/// the window, and two spellings of one file's size is one of them wrong. Decimal on
/// purpose — that is what a disk says — and bytes for the small files, because "0.0 GB" for
/// a 0-byte server is not a size.
fn size_of(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= 1_000.0 {
        format!("{:.0} kB", value / 1_000.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// The one line the Chat page needs when it cannot work: the first of the two pieces it
/// needs that is not there — the server binary, then the model file — or `None` when both
/// are there and the answer to "why can I not chat" is not a missing file at all.
///
/// The order is the order the two are needed in: a server with no model refuses to start,
/// and a model with no server is never loaded. One sentence, because the status row is one
/// line, and it is the *first* missing piece rather than a list of them: fixing that one
/// is what makes the next question different.
pub fn chat_missing() -> Option<String> {
    chat_missing_in(&models_dir(), &root().join("bin"))
}

/// [`chat_missing`] over named directories: the same decision, with the paths as
/// arguments, so a machine with nothing installed is a fixture and not a hypothetical.
pub fn chat_missing_in(models: &Path, bin: &Path) -> Option<String> {
    server_in(bin)
        .missing()
        .or_else(|| model_in(models).missing())
}
