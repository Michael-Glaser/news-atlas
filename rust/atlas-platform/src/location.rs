//! Where the data dir, the model and the rest of the layout are.
//!
//! One root, eleven derived paths, computed here and nowhere else. A crate that
//! needs "the archive" or "the model" asks this module; a second
//! `root.join("data").join("atlas.db")` anywhere in the workspace is a path rule
//! written twice, which is how the two spellings drift apart on a case-sensitive
//! file system.
//!
//! The layout is deliberately fixed and relative to the project, because the
//! promise is that ATLAS writes only into its own directory:
//!
//! ```text
//! <root>/data/config.json          settings
//! <root>/data/atlas.db             the archive (+ -wal, -shm)
//! <root>/data/app.log              the app log      (llama, chrome: same dir)
//! <root>/data/chrome-profile/      the browser's own profile
//! <root>/models/*.gguf             the local model, read only
//! <root>/bin/llama-server[.exe]    the inference binary, read only
//! <root>/public/                   the UI and the vendored geo data
//! ```
//!
//! `ATLAS_DATA_DIR` overrides the data directory and nothing else: a test (or a
//! second instance) needs a private archive, and moving the log and the browser
//! profile with it is the least surprising reading of "the data directory".

use std::path::{Path, PathBuf};

use crate::host;
use crate::root;

/// The model file name ATLAS looks for first. A configured name or any other
/// `.gguf` in `models/` wins; this is only the default the layout is built with.
pub const MODEL_EXTENSION: &str = ".gguf";

/// The root and the four directories under it that ATLAS writes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    /// The project root.
    pub root: PathBuf,
    /// `<root>/data` — the one directory ATLAS writes to.
    pub data_dir: PathBuf,
    /// `<root>/models` — read only.
    pub models_dir: PathBuf,
    /// `<root>/bin` — read only; the inference binary lives here.
    pub bin_dir: PathBuf,
}

impl Home {
    /// The layout under `root`.
    pub fn at(root: impl AsRef<Path>) -> Home {
        let root = root.as_ref().to_path_buf();
        Home {
            data_dir: data_dir(&root),
            models_dir: models_dir(&root),
            bin_dir: bin_dir(&root),
            root,
        }
    }

    /// The layout of this process: [`root::app_root`] as the root.
    ///
    /// `ATLAS_DATA_DIR` moves the archive and nothing else, which is what a test
    /// or a second instance needs. The root follows it only as far as the parent
    /// directory: `ATLAS_DATA_DIR=C:\atlas\data` gives the root `C:\atlas`, so
    /// `models/` and `bin/` still resolve against the project the data belongs
    /// to. A data dir whose name is not `data` keeps its own path but the root is
    /// still its parent — the two answers only differ from `root/data` when you
    /// asked them to.
    pub fn discover() -> Home {
        // A test or a second instance may point the archive somewhere else; the
        // root follows the data dir so a relative `models/` still resolves
        // against the project the data dir belongs to. Both halves of that rule
        // are pure functions below, so they are tested here rather than only at
        // boot.
        if let Ok(value) = std::env::var("ATLAS_DATA_DIR") {
            if let Some(data_dir) = data_dir_override(&value) {
                let root = root_for_data_dir(&data_dir).unwrap_or_else(root::app_root);
                let mut home = Home::at(root);
                home.data_dir = data_dir;
                return home;
            }
        }
        Home::at(root::app_root())
    }

    /// `data/config.json`.
    pub fn config_file(&self) -> PathBuf {
        self.data_dir.join("config.json")
    }

    /// `data/atlas.db`.
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("atlas.db")
    }

    /// `data/app.log`, `data/llama.log`, `data/chrome.log` — the log of `name`
    /// inside the data directory, so no crate invents its own log path.
    pub fn log(&self, name: &str) -> PathBuf {
        self.data_dir.join(format!("{}.log", name))
    }

    /// `data/chrome-profile` — the browser's private profile, inside `data/` so
    /// it inherits the 0700 directory instead of a world-readable `/tmp`.
    pub fn chrome_profile(&self) -> PathBuf {
        self.data_dir.join("chrome-profile")
    }

    /// `public/` — the UI and the vendored geo data.
    pub fn public_dir(&self) -> PathBuf {
        self.root.join("public")
    }

    /// The model file of `name` in `models/`.
    pub fn model_file(&self, name: &str) -> PathBuf {
        model_file(&self.models_dir, name)
    }

    /// The inference binary: `bin/llama-server[.exe]`.
    pub fn llama_server(&self) -> PathBuf {
        host::app_binary(&self.bin_dir, "llama-server")
    }
}

/// `<root>/data`.
pub fn data_dir(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join("data")
}

/// The data directory `ATLAS_DATA_DIR` names, or `None` when it names nothing.
///
/// Surrounding whitespace is trimmed: the value is usually typed into a shell or
/// a service file, and `ATLAS_DATA_DIR=" /srv/atlas/data"` used to name a
/// directory whose first character is a space. An empty value means "not set",
/// exactly as it did when this was read inline.
pub fn data_dir_override(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// The root a data directory belongs to: its parent directory.
///
/// `ATLAS_DATA_DIR=C:\atlas\data` gives the root `C:\atlas`, so `models/`, `bin/`
/// and `public/` still resolve against the project the data belongs to. A data
/// dir with no parent (`data`, `../data`) answers `None` — the caller falls back
/// to [`root::app_root`]. It used to answer `Some("")`, and an empty root made
/// every derived path relative to whatever directory the process started in,
/// which for a binary started from a shortcut is not the project at all.
pub fn root_for_data_dir(data_dir: &Path) -> Option<PathBuf> {
    match data_dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Some(parent.to_path_buf()),
        _ => None,
    }
}

/// `<root>/models`.
pub fn models_dir(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join("models")
}

/// `<root>/bin`.
pub fn bin_dir(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join("bin")
}

/// `models/<name>`.
pub fn model_file(models_dir: impl AsRef<Path>, name: &str) -> PathBuf {
    models_dir.as_ref().join(name)
}

/// The `.gguf` files in `models/`, newest name last, as a sorted list.
///
/// Sorted by name rather than modification time on purpose: a copied tree has no
/// meaningful mtimes, and two machines that hold the same files must pick the
/// same model. The caller takes the last one ("the newest"), which is the rule
/// `config.model_path()` already kept.
pub fn gguf_in(models_dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(models_dir.as_ref()) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_model_name(path))
        .collect();
    found.sort();
    found
}

/// Is this file name a model file? By extension, case-insensitively — a file
/// format, not a kind of content.
pub fn is_model_name(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(MODEL_EXTENSION.trim_start_matches('.')))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_one_root_plus_four_directories() {
        let home = Home::at("/p");
        assert_eq!(home.data_dir, Path::new("/p/data"));
        assert_eq!(home.models_dir, Path::new("/p/models"));
        assert_eq!(home.bin_dir, Path::new("/p/bin"));
        assert_eq!(home.config_file(), Path::new("/p/data/config.json"));
        assert_eq!(home.db(), Path::new("/p/data/atlas.db"));
        assert_eq!(home.log("chrome"), Path::new("/p/data/chrome.log"));
        assert_eq!(home.chrome_profile(), Path::new("/p/data/chrome-profile"));
        assert_eq!(home.public_dir(), Path::new("/p/public"));
        assert_eq!(
            home.model_file("m.gguf"),
            Path::new("/p/models/m.gguf")
        );
    }

    #[test]
    fn a_model_is_recognised_by_its_extension_in_any_case() {
        assert!(is_model_name(Path::new("m.gguf")));
        assert!(is_model_name(Path::new("M.GGUF")));
        assert!(!is_model_name(Path::new("m.gguf.part")));
        assert!(!is_model_name(Path::new("notes.txt")));
    }

    #[test]
    fn every_path_of_the_layout_is_derived_from_the_root() {
        // The guard against a build path baked into the binary: if any of these
        // ever came from a constant, one of them would stop being under the root
        // it was asked about. A path with spaces is included because a project
        // directory is allowed to have them.
        let root = Path::new("/tmp/atlas root/project");
        let home = Home::at(root);
        let derived = [
            home.data_dir.clone(),
            home.models_dir.clone(),
            home.bin_dir.clone(),
            home.config_file(),
            home.db(),
            home.log("app"),
            home.log("llama"),
            home.chrome_profile(),
            home.public_dir(),
            home.model_file("m.gguf"),
            home.llama_server(),
        ];
        for path in derived {
            assert!(
                path.starts_with(root),
                "{} is not under {}",
                path.display(),
                root.display()
            );
            let text = path.to_string_lossy();
            assert!(!text.contains("target"), "{} names a build directory", text);
        }
    }

    #[test]
    fn a_data_dir_override_names_its_own_parent_as_the_root() {
        // `ATLAS_DATA_DIR` may be written by hand, with a trailing separator, or
        // as a bare relative name. The rule is one function so it can be asked
        // here instead of only at boot: the root is the parent directory, and a
        // data dir with no parent has no root of its own.
        assert_eq!(data_dir_override(" /tmp/atlas-data "), Some(PathBuf::from("/tmp/atlas-data")));
        assert_eq!(data_dir_override(""), None);
        assert_eq!(data_dir_override("   "), None);
        assert_eq!(data_dir_override("/tmp/atlas-data").as_deref(), Some(Path::new("/tmp/atlas-data")));

        assert_eq!(root_for_data_dir(Path::new("/tmp/atlas-data")), Some(PathBuf::from("/tmp")));
        assert_eq!(
            root_for_data_dir(Path::new("/tmp/atlas-data/")),
            Some(PathBuf::from("/tmp")),
            "a trailing separator is the same directory"
        );
        assert_eq!(root_for_data_dir(Path::new("/data")), Some(PathBuf::from("/")));
        // Relative name, no parent: `Path::parent` answers `Some("")`, and an
        // empty root made every derived path (`models`, `bin`, `public`) resolve
        // against whatever directory the process happened to start in.
        assert_eq!(root_for_data_dir(Path::new("data")), None);
        assert_eq!(root_for_data_dir(Path::new("atlas-data")), None);
        // A root with a space in it is still one component.
        assert_eq!(
            root_for_data_dir(Path::new("/Users/Ann Marie/atlas/data")),
            Some(PathBuf::from("/Users/Ann Marie/atlas"))
        );
    }
}
