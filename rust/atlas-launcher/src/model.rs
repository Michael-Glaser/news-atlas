//! The model file: where ATLAS expects it, how big it is, and the exact sentence
//! when it is not there.
//!
//! ATLAS never downloads the GGUF. The file is placed by hand, once, and this
//! module's whole job is to find *the* file the app will load and to describe a
//! missing one so precisely that no one has to guess:
//!
//! * the configured path (`model_local` from `data/config.json` / `ATLAS_MODEL`),
//! * then `models/<MODEL_LOCAL_NAME>` — what `atlas-config` derives,
//! * then the newest other `.gguf` in `models/`, which is what the Python app did,
//!   because a user who copied a file with a different name should not be blocked.
//!
//! The upstream URL is *printed only*. Fetching it is the one thing this app does
//! not do; the sentence says so.

use std::path::{Path, PathBuf};

use atlas_config::{Config, Paths};

/// The model this process will load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub path: PathBuf,
    pub bytes: u64,
    /// True when the file is the configured one, false when it was found under
    /// another name in `models/`.
    pub configured: bool,
}

impl ModelFile {
    /// The line a person reads: name, size, and a note when the name is not the
    /// configured one.
    pub fn text(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        let mut text = format!(
            "{} ({}) at {}",
            name,
            crate::report::size(self.bytes),
            self.path.display()
        );
        if !self.configured {
            text.push_str(" — found under another name; set model_local to keep it stable");
        }
        text
    }
}

/// Why there is no model to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// Where ATLAS looked, and what to place there.
    pub expected: PathBuf,
    /// Other `.gguf` files in `models/` — empty in the ordinary case, and worth
    /// naming when a user put the file in the right directory under a wrong name.
    pub others: Vec<PathBuf>,
    /// The upstream hint. Printed, never fetched.
    pub upstream: String,
}

impl Missing {
    /// The whole sentence, in the order a user needs it: what is missing, what happens
    /// next, what to do.
    ///
    /// It used to say "Nothing is downloaded at run time — place the GGUF file there
    /// yourself", and that was true of the product: the model was a sentence and a manual
    /// step. It is not true any more — a double-clicked app provisions itself — and this
    /// sentence is what the doctor and the model step print, so it has to say what
    /// actually happens: `atlas start` runs the install once, and placing the file by hand
    /// stays a way to do it, not the only one.
    pub fn sentence(&self) -> String {
        let mut text = format!(
            "the local model is missing: {} does not exist. The next `atlas start` provisions \
             it once (`atlas-setup --install`, several gigabytes, the only time the app \
             touches the network); to place it yourself instead, the upstream copy is {} ; \
             that link is printed, never opened",
            self.expected.display(),
            self.upstream
        );
        if !self.others.is_empty() {
            let names: Vec<String> = self
                .others
                .iter()
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string())
                })
                .collect();
            text.push_str(&format!(
                ". Other GGUF files are in that directory: {} — either rename one to {}, or point \
                 model_local at it in data/config.json (or ATLAS_MODEL)",
                names.join(", "),
                self.expected
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "the expected name".to_string())
            ));
        }
        text
    }
}

/// Find the model file, or say exactly what is missing.
pub fn resolve(paths: &Paths, config: &Config) -> Result<ModelFile, Missing> {
    let configured = resolved_configured(paths, config);
    for candidate in configured.iter() {
        if let Some(model) = describe(candidate, true) {
            return Ok(model);
        }
    }
    // The file under any name: newest wins, the way `sorted(glob)[-1]` did, so an
    // older copy left behind by a model swap cannot win.
    let mut found = others(paths);
    found.sort();
    if let Some(newest) = found.pop() {
        if let Some(model) = describe(&newest, false) {
            return Ok(model);
        }
    }
    Err(Missing {
        expected: configured
            .first()
            .cloned()
            .unwrap_or_else(|| paths.model_file.clone()),
        others: others(paths),
        upstream: upstream(paths, config),
    })
}

/// The configured candidates, in the order they are tried.
pub fn resolved_configured(paths: &Paths, config: &Config) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let configured = config.model_local.trim();
    if !configured.is_empty() {
        out.push(PathBuf::from(configured));
    }
    if !out.iter().any(|path| path == &paths.model_file) {
        out.push(paths.model_file.clone());
    }
    out
}

/// Every `.gguf` directly inside `models/`.
pub fn others(paths: &Paths) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let entries = match std::fs::read_dir(&paths.models_dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_gguf = path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("gguf"))
            .unwrap_or(false);
        if is_gguf {
            out.push(path);
        }
    }
    out
}

fn describe(path: &Path, configured: bool) -> Option<ModelFile> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    Some(ModelFile {
        path: path.to_path_buf(),
        bytes: meta.len(),
        configured,
    })
}

fn upstream(paths: &Paths, config: &Config) -> String {
    let url = config.model_url.trim();
    if !url.is_empty() {
        return url.to_string();
    }
    let _ = paths;
    atlas_config::default_model_url()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-model-{}-{}-{}",
            tag,
            std::process::id(),
            atlas_store::now_iso().replace([':', '.'], "-")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("models"));
        dir
    }

    fn config_at(root: &Path) -> Config {
        Config::load_at(&Paths::at(root)).expect("a config can always be built")
    }

    #[test]
    fn an_empty_models_directory_says_exactly_what_is_missing() {
        let root = root("missing");
        let paths = Paths::at(&root);
        let config = config_at(&root);
        let missing = resolve(&paths, &config).unwrap_err();
        assert_eq!(missing.expected, paths.model_file);
        assert!(missing.others.is_empty());
        let sentence = missing.sentence();
        assert!(sentence.contains("the local model is missing"), "{}", sentence);
        assert!(sentence.contains(&paths.model_file.display().to_string()), "{}", sentence);
        // The sentence has to match the boot: since the product provisions itself, the
        // truth is "the next start fetches it once", not "nothing is downloaded".
        assert!(sentence.contains("provisions"), "{}", sentence);
        assert!(sentence.contains("huggingface.co"), "{}", sentence);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_under_another_name_is_found_and_named() {
        let root = root("alias");
        let paths = Paths::at(&root);
        let config = config_at(&root);
        std::fs::write(paths.models_dir.join("aaa.gguf"), vec![7u8; 1024]).unwrap();
        std::fs::write(paths.models_dir.join("zzz.gguf"), vec![7u8; 4096]).unwrap();
        // The last name in sorted order wins, which is what `sorted(glob)[-1]` did
        // in the Python: a leftover older file cannot shadow the one a user means.
        let model = resolve(&paths, &config).expect("the file under another name is found");
        assert_eq!(model.path.file_name().unwrap(), "zzz.gguf");
        assert!(!model.configured);
        assert_eq!(model.bytes, 4096);
        assert!(model.text().contains("another name"), "{}", model.text());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_configured_file_wins_over_anything_else_in_the_directory() {
        let root = root("configured");
        let paths = Paths::at(&root);
        std::fs::write(&paths.model_file, vec![1u8; 1024]).unwrap();
        std::fs::write(paths.models_dir.join("zzz.gguf"), vec![1u8; 4096]).unwrap();
        let config = config_at(&root);
        let model = resolve(&paths, &config).unwrap();
        assert_eq!(model.path, paths.model_file);
        assert!(model.configured);
        assert_eq!(model.bytes, 1024);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_zero_byte_file_is_not_a_model() {
        let root = root("empty");
        let paths = Paths::at(&root);
        let config = config_at(&root);
        std::fs::write(&paths.model_file, b"").unwrap();
        assert!(resolve(&paths, &config).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
