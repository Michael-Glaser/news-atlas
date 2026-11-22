//! A path as text, and where the project's own directories are.
//!
//! The rules are `atlas-platform`'s, and the shape Python published is kept:
//!
//! * **the text rules** — separators, a drive letter, UNC, the extended-length
//!   prefix, `..`, case folding — are [`atlas_platform::winpath`], so a
//!   Windows-shaped `data/config.json` is understood on every platform and the
//!   Windows rules are exercised by the tests that run here;
//! * **containment** ("inside the project", symlinks resolved) is
//!   [`atlas_platform::root`];
//! * **the layout** is [`atlas_platform::location`], so `data/`, `models/`, the
//!   model file and the `.exe` suffix exist once.
//!
//! What lives here: the *names* `config.py` published (`Paths`, `PathPolicy`'s
//! base, the JSON spelling of a path) and nothing that decides any of the above
//! a second time.
//!
//! It does not: decide which path a *setting* may become ([`crate::resolve`]),
//! set a file mode ([`crate::modes`]), or create a directory except when asked.

use std::path::{Path, PathBuf};

use atlas_platform::{host, location, root, winpath};

use crate::modes::{posix, DATA_MODE, FILE_MODE};

// ---------------------------------------------------------------------------
// path shape
// ---------------------------------------------------------------------------

/// Backslashes are accepted as separators so a `data/config.json` written on
/// Windows still resolves on POSIX (and the other way round). Windows accepts
/// both separators, so the reverse direction needs no special case.
pub(crate) fn separators_to_slash(text: &str) -> String {
    winpath::unify(text)
}

/// True for a path that starts outside any root: POSIX `/…`, a drive letter
/// `C:…`, or a UNC `\\server\share`.
pub(crate) fn is_absolute_shaped(text: &str) -> bool {
    use winpath::Shape;
    // A drive-relative path (`C:foo`) and a device path (`\\.\pipe\p`) count as
    // "not relative to the project" too: neither resolves under the root, and
    // both are refused rather than silently re-based.
    match winpath::shape(text) {
        Shape::Empty | Shape::Relative => false,
        Shape::Rooted
        | Shape::DriveAbsolute
        | Shape::DriveRelative
        | Shape::Unc
        | Shape::VerbatimDisk
        | Shape::VerbatimUnc
        | Shape::Device => true,
    }
}

/// Lexical normalisation: collapse `.`, `..` and repeated separators, without
/// touching the filesystem (Python's `os.path.normpath`).
pub(crate) fn normalize_slash(text: &str) -> String {
    winpath::normalise(text)
}

/// Trim trailing separators, keeping a bare `/`.
pub(crate) fn strip_trailing_slash(text: &str) -> String {
    let mut out = text.to_string();
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// `~` and `~/…` from the caller's environment. A missing `HOME` leaves the text
/// alone, as Python's `expanduser` does.
pub(crate) fn expand_user(text: &str) -> String {
    let rest = if text == "~" {
        Some("")
    } else if let Some(rest) = text.strip_prefix("~/") {
        Some(rest)
    } else if let Some(rest) = text.strip_prefix("~\\") {
        Some(rest)
    } else {
        None
    };
    let rest = match rest {
        Some(rest) => rest,
        None => return text.to_string(),
    };
    // `HOME` on POSIX, `USERPROFILE` on Windows: the platform's answer.
    let home = host::home_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    if home.is_empty() {
        return text.to_string();
    }
    if rest.is_empty() {
        return home;
    }
    // `Path::join`, not `format!("{}/{}")`: on Windows a home of `C:\Users\m` plus a
    // `/`-joined tail produced `C:\Users\m/rest`, which every consumer here then had to
    // normalise again (and Win32 accepts, so the bug was invisible). One join, in the
    // platform's own separator, and the callers' `separators_to_slash` still produces the
    // `/` form the JSON and the audit keys use.
    Path::new(&home).join(rest).to_string_lossy().to_string()
}

/// A path as the JSON output spells it: separators normalised, no escaping.
pub(crate) fn show(path: &Path) -> String {
    separators_to_slash(&path.to_string_lossy())
}

pub(crate) fn path_is_file(path: &str) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// True for a regular, executable file — the execute bit on POSIX, an executable
/// extension on Windows, decided by `atlas-platform` so the two conventions exist
/// once. A directory is never executable here, and neither is a dangling symlink.
pub fn is_executable(path: &str) -> bool {
    atlas_platform::executable::is_executable(path)
}

/// `path` with every symlink resolved as far as it exists. Components that do
/// not exist yet are appended lexically, so a not-yet-created model file still
/// gets a containment answer (Python's `os.path.realpath`).
pub(crate) fn real_path(text: &str) -> String {
    // `canonicalize` where it can, the missing tail appended lexically, the
    // extended-length prefix stripped: `atlas-platform`'s answer, because that
    // prefix is what made every path look like a symlink on Windows.
    root::canonical_text(Path::new(text))
}

/// Is `target` under `base` once `.` and `..` are collapsed? Case is folded
/// exactly where the file system folds it.
pub(crate) fn under(base: &str, target: &str) -> bool {
    winpath::inside(base, target, host::os())
}

/// Lexical containment: `target` under `base` with `..` collapsed, symlinks not
/// considered.
pub(crate) fn prefix_inside(base: &str, target: &str) -> bool {
    under(base, target)
}

/// Containment *after* resolving symlinks and, on Windows, case.
pub(crate) fn inside_real(base: &str, target: &str) -> bool {
    under(&real_path(base), &real_path(target))
}

// ---------------------------------------------------------------------------
// derived paths
// ---------------------------------------------------------------------------

/// Every path ATLAS derives from one root. Computed once, never configured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub config_file: PathBuf,
    pub models_dir: PathBuf,
    /// Where the model is expected: `models/<MODEL_LOCAL_NAME>`.
    pub model_file: PathBuf,
    pub db: PathBuf,
    pub db_wal: PathBuf,
    pub db_shm: PathBuf,
    pub log_app: PathBuf,
    pub log_llama: PathBuf,
    pub log_chrome: PathBuf,
    /// A stable Chrome profile directory under `data/`, so the reader browser
    /// keeps its cookies and logins between runs instead of starting from a
    /// throwaway directory — and so the 0700 mode `data/` already has covers it.
    pub chrome_profile: PathBuf,
    pub geo_local: PathBuf,
    pub geo_map_local: PathBuf,
    pub llama_local: PathBuf,
}

impl Paths {
    /// The layout under `root`.
    pub fn at(root: impl AsRef<Path>) -> Paths {
        Paths::of(location::Home::at(root))
    }

    /// The layout of an already-discovered [`location::Home`].
    ///
    /// **The data directory is the Home's, not `root.join("data")`.** `Home`
    /// carries an `ATLAS_DATA_DIR` override on purpose (a test, a second instance
    /// — `atlas-browser` already reads it that way), and re-deriving `<root>/data`
    /// from the Home's *root* silently dropped it: measured with
    /// `ATLAS_DATA_DIR=/tmp/atlas-archive`, `Home::discover()` answered
    /// `/tmp/atlas-archive` and `Paths::discover()` then answered `/tmp/data`, so
    /// the archive, the settings and every log were written to a directory nobody
    /// named. Nothing is silent here: the override is the path, or it is not set.
    fn of(home: location::Home) -> Paths {
        // The directories, the model file and the `.exe` suffix come from
        // `atlas-platform`'s layout; the files inside `data/` are this crate's
        // names for them, so a caller keeps the keys it always read.
        let data = home.data_dir.clone();
        Paths {
            data_dir: data.clone(),
            config_file: home.config_file(),
            model_file: home.model_file(crate::MODEL_LOCAL_NAME),
            models_dir: home.models_dir.clone(),
            db: home.db(),
            db_wal: data.join("atlas.db-wal"),
            db_shm: data.join("atlas.db-shm"),
            log_app: home.log("app"),
            log_llama: home.log("llama"),
            log_chrome: home.log("chrome"),
            chrome_profile: home.chrome_profile(),
            geo_local: home
                .public_dir()
                .join("vendor")
                .join("countries.geo.json"),
            geo_map_local: home
                .public_dir()
                .join("vendor")
                .join("ne_50m_countries.geojson"),
            llama_local: home.llama_server(),
            root: home.root,
        }
    }

    /// The project root for this process, and the data directory it carries.
    ///
    /// The binary lives in `rust/target/release`, so the root is the first
    /// ancestor that looks like the project (has `rust/Cargo.toml` or `server/`
    /// or `data/`). A binary somewhere else falls back to the working directory,
    /// which is what the Python spawn sites already set to the project root.
    pub fn discover() -> Paths {
        // `current_exe` then the working directory, up to eight ancestors: one
        // walk, in `atlas-platform` — and `ATLAS_DATA_DIR` honoured there, which
        // is why the Home is passed on whole instead of being reduced to its root.
        Paths::of(location::Home::discover())
    }

    /// `data/config.json` as a message spells it.
    pub fn config_origin(&self) -> String {
        "data/config.json".to_string()
    }

    /// Every path, as JSON, for the `paths` subcommand and `/api/status`.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value};
        let mut map = Map::new();
        let pairs: [(&str, &PathBuf); 15] = [
            ("root", &self.root),
            ("data_dir", &self.data_dir),
            ("config_file", &self.config_file),
            ("models_dir", &self.models_dir),
            ("model_file", &self.model_file),
            ("db", &self.db),
            ("db_wal", &self.db_wal),
            ("db_shm", &self.db_shm),
            ("log_app", &self.log_app),
            ("log_llama", &self.log_llama),
            ("log_chrome", &self.log_chrome),
            ("chrome_profile", &self.chrome_profile),
            ("geo_local", &self.geo_local),
            ("geo_map_local", &self.geo_map_local),
            ("llama_local", &self.llama_local),
        ];
        for (name, path) in pairs {
            map.insert(name.to_string(), Value::String(show(path)));
        }
        map.insert("posix".to_string(), Value::Bool(posix()));
        map.insert(
            "data_mode".to_string(),
            Value::String(mode_text(DATA_MODE)),
        );
        map.insert(
            "file_mode".to_string(),
            Value::String(mode_text(FILE_MODE)),
        );
        Value::Object(map)
    }
}

pub(crate) fn mode_text(mode: u32) -> String {
    if posix() {
        format!("0o{:o}", mode)
    } else {
        // No mode on Windows: the ACL inherited from the profile is the answer.
        "windows-acl".to_string()
    }
}

/// `{path, exists, size}` for one file — what `describe` reports per path.
pub(crate) fn file_info(path: &str) -> serde_json::Value {
    use serde_json::{Map, Value};
    let mut map = Map::new();
    map.insert("path".to_string(), Value::String(path.to_string()));
    let (exists, size) = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => (true, meta.len()),
        _ => (false, 0),
    };
    map.insert("exists".to_string(), Value::Bool(exists));
    map.insert("size".to_string(), Value::from(size));
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use serde_json::Value;
    use crate::config::model_path;
    use crate::spec::MODEL_LOCAL_NAME;


#[test]
fn derived_paths_live_under_the_root() {
    let dir = TempDir::new("paths");
    let paths = dir.at();
    assert_eq!(paths.data_dir, dir.path.join("data"));
    assert_eq!(paths.config_file, dir.path.join("data").join("config.json"));
    assert_eq!(paths.db, dir.path.join("data").join("atlas.db"));
    assert_eq!(paths.db_wal, dir.path.join("data").join("atlas.db-wal"));
    assert_eq!(paths.db_shm, dir.path.join("data").join("atlas.db-shm"));
    assert_eq!(paths.log_app, dir.path.join("data").join("app.log"));
    assert_eq!(paths.log_llama, dir.path.join("data").join("llama.log"));
    assert_eq!(paths.log_chrome, dir.path.join("data").join("chrome.log"));
    assert_eq!(paths.chrome_profile, dir.path.join("data").join("chrome-profile"));
    assert_eq!(paths.models_dir, dir.path.join("models"));
    assert!(paths.model_file.ends_with(MODEL_LOCAL_NAME));
    assert_eq!(
        paths.geo_local,
        dir.path.join("public").join("vendor").join("countries.geo.json")
    );
    assert!(paths.llama_local.starts_with(dir.path.join("bin")));
    let json = paths.to_json();
    assert_eq!(json.get("data_dir").and_then(Value::as_str), Some(
        crate::paths::show(&paths.data_dir).as_str()
    ));
    assert!(json.get("chrome_profile").is_some());
}



#[test]
fn an_executable_is_only_what_is_really_executable() {
    let dir = TempDir::new("exec");
    dir.write("script.sh", "#!/bin/sh\n");
    let script = crate::paths::show(&dir.path.join("script.sh"));
    assert!(!is_executable(&script), "a non-executable file is not one");
    #[cfg(unix)]
    {
        // Brought in here, not at the top of the test module: `PathPolicy` and
        // `resolve_binary` are asked about an *executable* file, which only POSIX has
        // a bit for, and an import that only one platform uses is an unused-import
        // warning on the other.
        use crate::resolve::{resolve_binary, PathPolicy};
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable(&script));
        assert!(is_executable(&script));
        let mut policy = PathPolicy::pinned(&dir.path, "", "binary");
        policy.allow_outside = false;
        assert_eq!(resolve_binary(Some("script.sh"), &policy), script);
    }
    assert!(!is_executable(""));
    assert!(!is_executable(&crate::paths::show(&dir.path)));
}


#[test]
fn the_newest_gguf_is_used_when_nothing_is_configured() {
    let dir = TempDir::new("model-pick");
    dir.write("models/b.gguf", "x");
    dir.write("models/a.gguf", "x");
    dir.write("models/notes.txt", "x");
    let config = load_env_free(&dir).expect("load");
    // The default points at the expected name, which does not exist here, so the
    // newest `.gguf` in `models/` wins.
    assert!(config.model_local.ends_with(MODEL_LOCAL_NAME));
    assert!(!std::path::Path::new(&config.model_local).exists());
    assert_eq!(
        model_path(&config, &dir.at()),
        format!("{}/models/b.gguf", crate::paths::show(&dir.path))
    );
}


#[test]
fn paths_discovery_lands_on_a_project_root() {
    let paths = Paths::discover();
    assert!(
        paths.root.join("rust").join("Cargo.toml").is_file() || paths.root.join("server").is_dir(),
        "discovered {}",
        paths.root.display()
    );
    assert!(paths.data_dir.ends_with("data"));
    assert_eq!(posix(), cfg!(unix));
    assert_eq!(DATA_MODE, 0o700);
    assert_eq!(FILE_MODE, 0o600);
    assert!(paths.config_origin().ends_with("data/config.json"));
}
}
