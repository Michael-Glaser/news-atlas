//! The settings a reader may change from the window, as rules that name what is wrong.
//!
//! Decisions:
//!
//! * **which keys the queue may set** — [`SETTABLE`] — and that every other key is refused
//!   *by name*, never dropped. `Config::save` stores what it is handed (minus the keys it
//!   refuses for other reasons), so a typo in a settings request would otherwise become a
//!   setting nobody reads and a reader who believes it took effect;
//! * **a model path** is absolute, ends in `.gguf`, exists, is a regular file, and resolves —
//!   through its own symlinks — into `<root>/models`. That is the documented choice: an
//!   absolute path *outside* the project is refused, because [`crate::Config::save`] resolves
//!   the value with a policy that refuses a climb-out and stores `""` instead
//!   (`crate::resolve::PathPolicy::model`), so accepting one here would be a promise the
//!   settings file cannot keep. A model from elsewhere comes in through the import field,
//!   whose *source* may be anywhere the reader can read;
//! * **an imported address** is `https://…` or an absolute `.gguf` path — never `file:`,
//!   never `javascript:`, never plain `http:`, never a credential in the URL, never a
//!   relative path;
//! * **an integer setting** is clamped to its row of the SPEC table ([`crate::spec`]), and
//!   the clamp is reported as a note ([`crate::warn`]) rather than done silently.
//!
//! It does not: write `data/config.json` ([`crate::Config::save`] does, atomically, 0600, in
//! a 0700 directory), build the llama.cpp argv (one builder, in `atlas-launcher`), download
//! anything, or start a process. Every sentence here names the part that is wrong and the
//! thing to do instead, because a refusal is read by a person.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::coerce::{coerce, Clamp};
use crate::paths::{
    expand_user, is_absolute_shaped, normalize_slash, prefix_inside, real_path,
    separators_to_slash, show, strip_trailing_slash,
};
use crate::spec::{number_value, number_text, range_text, requirement_text, spec_of};

/// The keys a settings request may set: the model path and the five numbers the launcher's
/// argv is built from.
///
/// `search_engine` is *not* here. It lives in the archive's `meta` table, is written by the
/// agent's own gate (the one the search tool asks), and mixing the two in one request would
/// mean one row that half-applies. A request naming it is sent on its own.
pub const SETTABLE: [&str; 6] = [
    "model_local",
    "model_context",
    "model_reasoning",
    "model_answer",
    "model_gpu_layers",
    "model_threads",
];

/// The one suffix llama.cpp reads.
pub const MODEL_SUFFIX: &str = ".gguf";

/// The key that belongs to the archive rather than to `data/config.json`.
pub const ARCHIVE_KEY: &str = "search_engine";

/// What a settings request becomes: the overrides to save, and the notes about what changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overrides {
    /// Key/value pairs ready for [`crate::Config::save`].
    pub map: Map<String, Value>,
    /// One sentence per value the range had to move. Empty when nothing was clamped.
    pub notes: Vec<String>,
}

/// Does this request carry anything `data/config.json` owns?
///
/// A request that names only `search_engine` is the archive's and is not ours; everything
/// else is answered here (including a request that names nothing, which is refused by name).
pub fn is_settings_request(settings: &Map<String, Value>) -> bool {
    settings.keys().any(|key| key != ARCHIVE_KEY)
}

/// Validate one settings map into the overrides `Config::save` takes.
///
/// The order is the order a person reads: unknown keys first (they are the reader's typo),
/// then the path, then the numbers.
pub fn model_settings(settings: &Map<String, Value>, root: &Path) -> Result<Overrides, String> {
    let mut unknown: Vec<&str> = settings
        .keys()
        .map(String::as_str)
        .filter(|key| !SETTABLE.contains(key) && *key != ARCHIVE_KEY)
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(format!(
            "data/config.json has no setting called {}; this request can set {} — nothing is \
             dropped silently, so nothing was written",
            quoted(&unknown),
            SETTABLE.join(", ")
        ));
    }
    if settings.contains_key(ARCHIVE_KEY) {
        return Err(format!(
            "{ARCHIVE_KEY} lives in the archive's meta table, not in data/config.json; send it \
             in its own request — nothing was written"
        ));
    }
    let mut overrides = Overrides::default();
    if let Some(value) = settings.get("model_local") {
        let Value::String(text) = value else {
            return Err(format!(
                "model_local is a path and takes a string, not {}",
                shape_of(value)
            ));
        };
        let resolved = model_local_path(text, root)?;
        overrides
            .map
            .insert("model_local".to_string(), Value::String(show(&resolved)));
    }
    for key in SETTABLE {
        if key == "model_local" {
            continue;
        }
        let Some(raw) = settings.get(key) else {
            continue;
        };
        // The row is the same one the CLI validates against, so a value this request accepts
        // is a value `atlas-config` accepts; there is no second table of ranges.
        let Some(spec) = spec_of(key) else {
            return Err(format!(
                "{key} has no row in ATLAS's settings table: this is a bug in the build, not \
                 in your request, and nothing was written"
            ));
        };
        let Some(coerced) = coerce(spec, Some(raw)) else {
            return Err(format!(
                "{key} = {} is not usable: {}",
                text_of(raw),
                requirement_text(spec)
            ));
        };
        if coerced.clamp != Clamp::None {
            // Clamped, not refused — and said out loud, through the notes mechanism every
            // other setting uses. A reader whose 999 999 context becomes 262 144 has to be
            // able to explain the number the app runs with.
            overrides.notes.push(format!(
                "{key} = {} was out of range and was clamped to {} ({} is the range)",
                text_of(raw),
                number_text(spec, coerced.value),
                range_text(spec)
            ));
        }
        overrides
            .map
            .insert(key.to_string(), number_value(*spec, coerced.value));
    }
    Ok(overrides)
}

/// The path a `model_local` value may become, or the sentence saying why it may not.
///
/// `root` is the project root; the model directory is `<root>/models`.
pub fn model_local_path(value: &str, root: &Path) -> Result<PathBuf, String> {
    let text = value.trim();
    if text.is_empty() {
        return Err(
            "the model path is empty: write the absolute path of the .gguf file ATLAS should \
             serve, or leave model_local out and ATLAS keeps the model it ships with"
                .to_string(),
        );
    }
    if text.contains('\0') {
        return Err(
            "the model path carries a NUL byte, which no file name can contain".to_string(),
        );
    }
    let shaped = separators_to_slash(&expand_user(text));
    if !is_absolute_shaped(&shaped) {
        // Relative is ambiguous in exactly the way that makes it unsafe: the window and the
        // agent are two processes with two working directories, so `models/x.gguf` means two
        // files. The path is stored, so it has to mean the one the reader saw.
        return Err(format!(
            "the model path has to be absolute (it starts with / or a drive letter), not \
             {shaped:?}: a relative path means one file to the window and another to the agent"
        ));
    }
    if !shaped.to_ascii_lowercase().ends_with(MODEL_SUFFIX) {
        return Err(format!(
            "the model path has to end in {MODEL_SUFFIX}: llama.cpp reads GGUF files, and a \
             path without the suffix is usually a directory or a program"
        ));
    }
    let path = PathBuf::from(&shaped);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "no model file at {}: write the path of a file that exists, or import one first",
                show(&path)
            ))
        }
        Err(error) => {
            return Err(format!(
                "the model file at {} cannot be read ({})",
                show(&path),
                error.kind()
            ))
        }
        Ok(meta) if meta.is_dir() => {
            return Err(format!(
                "the model path is a directory, not a {MODEL_SUFFIX} file: {}",
                show(&path)
            ))
        }
        Ok(_) => {}
    }
    let real = std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "the model file at {} cannot be resolved ({})",
            show(&path),
            error.kind()
        )
    })?;
    // The model has to be a file inside `<root>/models`, with its symlinks resolved first:
    // otherwise `data/config.json` (a file a backup or a sync client can swap) would name any
    // file the process can read, and llama.cpp would be started on it.
    //
    // Both sides of the comparison are resolved first. A macOS temp directory is `/var/…` as a
    // name and `/private/var/…` once resolved, so comparing a lexical root with a canonical
    // file would refuse every model in the project (measured: it did, which is how this line
    // came to be).
    //
    // A model kept on another volume is *not* accepted, and that is the documented choice, not
    // an oversight: `Config::save` resolves the value through `resolve_path`, whose policy for
    // a model refuses anything that climbs out of the project and stores `""` instead
    // (`resolve.rs`, `PathPolicy::model`). Accepting an outside path here would be a promise
    // the settings file cannot keep — so the sentence sends the reader to `models/`, and the
    // import field is where a file from elsewhere comes in.
    let lexical_root = strip_trailing_slash(&separators_to_slash(&show(root)));
    let models = format!("{}/models", strip_trailing_slash(&real_path(&lexical_root)));
    let real_text = normalize_slash(&real_path(&show(&real)));
    if !prefix_inside(&models, &real_text) {
        return Err(format!(
            "the model has to be a file inside {models}, and this path resolves to {real_text}: \
             a link there would let the settings file name a file outside models/ — import the \
             file into models/ first"
        ));
    }
    Ok(real)
}

/// What an import field may name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Import {
    /// An `https://` address to fetch the file from.
    Address(String),
    /// An absolute path to a `.gguf` the reader already has.
    File(PathBuf),
}

/// The address or file an import field may become, or the sentence saying why it may not.
///
/// The rules are structural: a scheme, a suffix, a shape of path. Nothing here reads a word
/// to decide what the text is about.
pub fn import_target(value: &str) -> Result<Import, String> {
    let text = value.trim();
    if text.is_empty() {
        return Err(
            "the import field is empty: paste an https:// address of a .gguf file, or the \
             absolute path of one you already have"
                .to_string(),
        );
    }
    if text.contains('\0') {
        return Err("the import field carries a NUL byte, which no address and no file name \
                    can contain"
            .to_string());
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("https://") {
        let after = &text["https://".len()..];
        let authority = after.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            // `https://user:password@host/…`: userinfo is a credential whatever its shape, so
            // this is asked *first* — the reader is told about the part they can see, and a
            // password that is too short for the token rule is still a password.
            return Err(
                "that address carries a user name and password in it; ATLAS will not store it \
                 — remove them from the address and try again"
                    .to_string(),
            );
        }
        if authority.is_empty() || authority.starts_with(':') {
            return Err("that address has no host after https://".to_string());
        }
        if text.chars().any(char::is_whitespace) {
            return Err(
                "that address has a space in it: an address cannot carry one".to_string(),
            );
        }
        // A credential the address carries anywhere else — a key in its query, a token in its
        // fragment: the reader is handing us a secret, and it would be copied into the log
        // line, the settings file and the row. The rule is `atlas-guard`'s, asked here as the
        // same question the store's doors ask a value.
        if atlas_guard::redact::credential_in(text).is_some() {
            return Err(
                "that address carries a credential; ATLAS will not store it — remove it from \
                 the address and try again"
                    .to_string(),
            );
        }
        return Ok(Import::Address(text.to_string()));
    }
    // Every other scheme is refused, and named: `file:` names a path on whichever machine
    // reads it, `javascript:` is not a file at all, and plain `http:` would fetch a model
    // over a connection anyone on the path can rewrite. A single letter before the colon is
    // a Windows drive, not a scheme (`C:/models/x.gguf`), which is why the length is checked.
    if let Some((scheme, _)) = text.split_once(':') {
        let scheme_shaped = scheme.len() > 1
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
        if scheme_shaped {
            let named = scheme.to_ascii_lowercase();
            return Err(match named.as_str() {
                "http" => "a plain http:// address is refused: the file would travel in the \
                           clear, where anyone on the path can replace it — use https://"
                    .to_string(),
                "file" => "a file:// address is refused: it names a path on whichever machine \
                           reads it, not a file ATLAS can fetch — write the absolute path, or \
                           an https:// address"
                    .to_string(),
                "javascript" | "data" => format!(
                    "a {named}: address is refused: ATLAS imports a .gguf file over https:// \
                     or from an absolute path, and {named}: is neither"
                ),
                other => format!(
                    "a {other}: address is refused: ATLAS imports a .gguf file over https:// \
                     or from an absolute path"
                ),
            });
        }
    }
    let shaped = separators_to_slash(&expand_user(text));
    if !is_absolute_shaped(&shaped) {
        return Err(format!(
            "the import path has to be absolute (it starts with / or a drive letter), not \
             {shaped:?}: a relative path means one file to the window and another to the agent"
        ));
    }
    if !shaped.to_ascii_lowercase().ends_with(MODEL_SUFFIX) {
        return Err(format!(
            "the import path has to end in {MODEL_SUFFIX}: llama.cpp reads GGUF files"
        ));
    }
    let path = PathBuf::from(&shaped);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "no file to import at {}: write the path of a file that exists",
                show(&path)
            ))
        }
        Err(error) => {
            return Err(format!(
                "the file at {} cannot be read ({})",
                show(&path),
                error.kind()
            ))
        }
        Ok(meta) if meta.is_dir() => {
            return Err(format!(
                "the import path is a directory, not a {MODEL_SUFFIX} file: {}",
                show(&path)
            ))
        }
        Ok(_) => {}
    }
    let real = std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "the file at {} cannot be resolved ({})",
            show(&path),
            error.kind()
        )
    })?;
    Ok(Import::File(real))
}

/// `"a", "b"` — the keys of a sentence, in the order they are read.
fn quoted(keys: &[&str]) -> String {
    keys.iter()
        .map(|key| format!("\"{key}\""))
        .collect::<Vec<String>>()
        .join(", ")
}

/// What a JSON value is, for a sentence that has to name it.
fn shape_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true/false",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// The value as a person reads it, quoted and short: it goes into a sentence, not a log.
fn text_of(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let clipped: String = text.chars().take(40).collect();
    if text.chars().count() > 40 {
        format!("\"{clipped}…\"")
    } else {
        format!("\"{clipped}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TempDir;

    /// A project root with a `models/` directory and one model file in it.
    fn project(tag: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(tag);
        let models = dir.path.join("models");
        std::fs::create_dir_all(&models).expect("models");
        let model = models.join("local.gguf");
        std::fs::write(&model, b"GGUF").expect("model");
        (dir, model)
    }

    fn settings(pairs: &[(&str, Value)]) -> Map<String, Value> {
        let mut map = Map::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), value.clone());
        }
        map
    }

    // ---- the model path: one test per refusal, each asserting the sentence ----

    #[test]
    fn an_empty_model_path_is_refused_with_a_sentence() {
        let (dir, _) = project("checks-empty");
        let error = model_local_path("  ", &dir.path).expect_err("refused");
        assert!(error.contains("the model path is empty"), "{error}");
        assert!(error.contains("absolute path of the .gguf"), "{error}");
    }

    #[test]
    fn a_relative_model_path_is_refused_because_two_processes_would_read_it_differently() {
        let (dir, _) = project("checks-relative");
        let error = model_local_path("models/local.gguf", &dir.path).expect_err("refused");
        assert!(error.contains("has to be absolute"), "{error}");
        assert!(error.contains("another to the agent"), "{error}");
    }

    #[test]
    fn a_model_path_that_is_not_a_gguf_is_refused() {
        let (dir, _) = project("checks-suffix");
        let script = dir.path.join("models/run.sh");
        std::fs::write(&script, b"#!/bin/sh\n").expect("write");
        let error = model_local_path(&show(&script), &dir.path).expect_err("refused");
        assert!(error.contains("has to end in .gguf"), "{error}");
        assert!(!error.contains("run.sh"), "the refusal names the rule, not the shell script");
    }

    #[test]
    fn a_model_path_with_a_nul_byte_is_refused() {
        let (dir, _) = project("checks-nul");
        let error = model_local_path("/tmp/a\0b.gguf", &dir.path).expect_err("refused");
        assert!(error.contains("NUL byte"), "{error}");
        assert!(error.contains("no file name can contain"), "{error}");
    }

    #[test]
    fn a_model_path_that_does_not_exist_is_refused() {
        let (dir, _) = project("checks-missing");
        let missing = dir.path.join("models/not-there.gguf");
        let error = model_local_path(&show(&missing), &dir.path).expect_err("refused");
        assert!(error.contains("no model file at"), "{error}");
        assert!(error.contains("import one first"), "{error}");
    }

    #[test]
    fn a_directory_named_like_a_model_is_refused() {
        let (dir, _) = project("checks-dir");
        let disguised = dir.path.join("models/folder.gguf");
        std::fs::create_dir_all(&disguised).expect("dir");
        let error = model_local_path(&show(&disguised), &dir.path).expect_err("refused");
        assert!(error.contains("is a directory, not a .gguf file"), "{error}");
    }

    #[test]
    fn a_model_inside_the_project_has_to_sit_in_models() {
        let (dir, _) = project("checks-outside-models");
        let elsewhere = dir.path.join("data").join("sneaky.gguf");
        std::fs::create_dir_all(dir.path.join("data")).expect("data");
        std::fs::write(&elsewhere, b"GGUF").expect("write");
        let error = model_local_path(&show(&elsewhere), &dir.path).expect_err("refused");
        assert!(error.contains("has to be a file inside"), "{error}");
        assert!(error.contains("models"), "{error}");
        assert!(error.contains("import the file into models/ first"), "{error}");
        // The file that is in `models/` is accepted, and comes back as one path.
        let (dir2, model) = project("checks-inside-models");
        assert_eq!(
            model_local_path(&show(&model), &dir2.path).expect("accepted"),
            std::fs::canonicalize(&model).expect("canonical")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_models_is_refused_after_it_is_resolved() {
        let (dir, _) = project("checks-link");
        let outside = TempDir::new("checks-link-outside");
        let target = outside.path.join("secret.gguf");
        std::fs::write(&target, b"GGUF").expect("target");
        let link = dir.path.join("models/link.gguf");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let error = model_local_path(&show(&link), &dir.path).expect_err("refused");
        assert!(error.contains("resolves to"), "{error}");
        assert!(error.contains("outside models/"), "{error}");
        // A link inside `models/` that stays inside `models/` is a file like any other.
        let inner = dir.path.join("models/real.gguf");
        std::fs::write(&inner, b"GGUF").expect("inner");
        let good = dir.path.join("models/alias.gguf");
        std::os::unix::fs::symlink(&inner, &good).expect("symlink");
        assert!(model_local_path(&show(&good), &dir.path).is_ok());
    }

    #[test]
    fn a_model_on_another_volume_is_refused_and_the_sentence_names_models() {
        // The documented choice, and the reason it is this one: `Config::save` would resolve an
        // outside path to "" (`PathPolicy::model`), so a value accepted here would not survive
        // the write. `tests/settings_safety.rs` pins the write itself.
        let outside = TempDir::new("checks-elsewhere");
        let model = outside.path.join("big.gguf");
        std::fs::write(&model, b"GGUF").expect("model");
        let (dir, _) = project("checks-elsewhere-root");
        let error = model_local_path(&show(&model), &dir.path).expect_err("refused");
        assert!(error.contains("has to be a file inside"), "{error}");
        assert!(error.contains("/models"), "the sentence names the directory: {error}");
        assert!(error.contains("big.gguf"), "{error}");
        // The same file after an import lands in `models/` and is accepted.
        let copied = dir.path.join("models/big.gguf");
        std::fs::copy(&model, &copied).expect("copy");
        assert!(model_local_path(&show(&copied), &dir.path).is_ok());
    }

    // ---- the import field ----

    #[test]
    fn an_https_address_with_a_credential_is_refused() {
        // A key in the query: nothing in the address names it as a secret, so the shape rule
        // is what sees it.
        let error = import_target("https://models.example/qwen.gguf?api_key=sk-live-abcdefghijklmnop")
            .expect_err("refused");
        assert!(error.contains("carries a credential"), "{error}");
        assert!(error.contains("ATLAS will not store it"), "{error}");
        assert!(!error.contains("sk-live"), "the refusal repeats no value: {error}");
    }

    #[test]
    fn a_user_name_and_password_in_an_address_are_refused_before_anything_shaped_is_looked_for()
    {
        // `hunter2` is too short for the token rule; userinfo is a credential by position, so
        // this sentence is the one the reader gets, and it repeats nothing.
        let error = import_target("https://alice:hunter2@models.example/qwen.gguf")
            .expect_err("refused");
        assert!(error.contains("user name and password"), "{error}");
        assert!(!error.contains("hunter2"), "{error}");
        // A URL with both a user name and a key in the query is still told about the part a
        // person can see first.
        let both = import_target(
            "https://user:secret@models.example/qwen.gguf?api_key=sk-live-abcdefghijklmnop",
        )
        .expect_err("refused");
        assert!(both.contains("user name and password"), "{both}");
    }

    #[test]
    fn plain_http_is_refused_and_the_sentence_names_https() {
        let error = import_target("http://models.example/qwen.gguf").expect_err("refused");
        assert!(error.starts_with("a plain http:// address is refused"), "{error}");
        assert!(error.contains("use https://"), "{error}");
    }

    #[test]
    fn file_and_javascript_and_data_addresses_are_refused() {
        for (value, named) in [
            ("file:///etc/passwd", "file:"),
            ("javascript:alert(1)", "javascript:"),
            ("data:application/octet-stream;base64,AAAA", "data:"),
            ("ftp://models.example/qwen.gguf", "ftp:"),
        ] {
            let error = import_target(value).expect_err(value);
            assert!(
                error.contains(named),
                "{value}: the sentence has to name the scheme: {error}"
            );
            assert!(error.contains("https://"), "{value}: {error}");
        }
    }

    #[test]
    fn a_relative_import_path_is_refused() {
        let error = import_target("downloads/qwen.gguf").expect_err("refused");
        assert!(error.contains("has to be absolute"), "{error}");
        assert!(error.contains("another to the agent"), "{error}");
    }

    #[test]
    fn an_import_path_that_is_not_a_model_is_refused() {
        let dir = TempDir::new("checks-import-suffix");
        let note = dir.path.join("notes.txt");
        std::fs::write(&note, b"hello").expect("write");
        let error = import_target(&show(&note)).expect_err("refused");
        assert!(error.contains("has to end in .gguf"), "{error}");
    }

    #[test]
    fn an_https_address_and_an_absolute_gguf_are_accepted() {
        assert_eq!(
            import_target("https://models.example/qwen.gguf").expect("accepted"),
            Import::Address("https://models.example/qwen.gguf".to_string())
        );
        let (_dir, model) = project("checks-import-ok");
        assert_eq!(
            import_target(&show(&model)).expect("accepted"),
            Import::File(std::fs::canonicalize(&model).expect("canonical"))
        );
    }

    // ---- the five numbers, through the SPEC table the CLI uses ----

    #[test]
    fn an_unknown_key_is_refused_by_name_and_nothing_is_written() {
        let (dir, _) = project("checks-unknown");
        let map = settings(&[("model_contex", Value::from(8192))]);
        let error = model_settings(&map, &dir.path).expect_err("refused");
        assert!(error.contains("\"model_contex\""), "{error}");
        assert!(error.contains("nothing is dropped silently"), "{error}");
        assert!(error.contains("model_context"), "the sentence lists what can be set: {error}");
    }

    #[test]
    fn a_key_of_the_archive_in_this_request_is_refused_and_told_where_it_belongs() {
        let (dir, _) = project("checks-archive-key");
        let map = settings(&[
            ("model_answer", Value::from(4096)),
            ("search_engine", Value::String("https://s.example/?q={query}".into())),
        ]);
        let error = model_settings(&map, &dir.path).expect_err("refused");
        assert!(error.contains("meta table"), "{error}");
        assert!(error.contains("its own request"), "{error}");
        assert_eq!(
            is_settings_request(&settings(&[(
                "search_engine",
                Value::String("x".into())
            )])),
            false,
            "a search_engine-only request is the archive's, not config.json's"
        );
    }

    #[test]
    fn a_number_out_of_range_is_clamped_by_the_spec_row_and_the_note_says_so() {
        let (dir, _) = project("checks-clamp");
        let map = settings(&[
            ("model_context", Value::from(999_999)),
            ("model_gpu_layers", Value::from(-7)),
            ("model_answer", Value::from(" 4096 ")),
        ]);
        let overrides = model_settings(&map, &dir.path).expect("accepted");
        assert_eq!(overrides.map["model_context"], Value::from(262_144));
        assert_eq!(overrides.map["model_gpu_layers"], Value::from(-1));
        assert_eq!(overrides.map["model_answer"], Value::from(4096));
        assert_eq!(overrides.notes.len(), 2, "{:?}", overrides.notes);
        assert!(overrides.notes[0].contains("model_context"), "{:?}", overrides.notes);
        assert!(overrides.notes[0].contains("262144"), "{:?}", overrides.notes);
        assert!(overrides.notes[0].contains("1024 to 262144"), "{:?}", overrides.notes);
        assert!(overrides.notes[1].contains("model_gpu_layers"), "{:?}", overrides.notes);
        assert!(overrides.notes[1].contains("-1 to 999"), "{:?}", overrides.notes);
    }

    #[test]
    fn a_value_that_is_not_a_number_is_refused_with_the_rows_own_requirement() {
        let (dir, _) = project("checks-banana");
        let error = model_settings(&settings(&[("model_context", Value::from(true))]), &dir.path)
            .expect_err("refused");
        assert!(error.contains("model_context = \"true\" is not usable"), "{error}");
        assert!(error.contains("write a whole number from 1024 to 262144"), "{error}");
        // A missing value inside the range is not clamped and produces no note.
        let overrides = model_settings(&settings(&[("model_threads", Value::from(0))]), &dir.path)
            .expect("accepted");
        assert!(overrides.notes.is_empty(), "{:?}", overrides.notes);
        assert_eq!(overrides.map["model_threads"], Value::from(0));
    }

    #[test]
    fn the_model_path_travels_as_a_string_and_is_checked_first() {
        let (dir, model) = project("checks-path-in-settings");
        let overrides = model_settings(
            &settings(&[
                ("model_local", Value::String(show(&model))),
                ("model_context", Value::from(65536)),
            ]),
            &dir.path,
        )
        .expect("accepted");
        assert_eq!(
            overrides.map["model_local"],
            Value::String(show(&std::fs::canonicalize(&model).expect("canonical")))
        );
        // A non-string path is refused rather than coerced: a number is never a file.
        let error = model_settings(&settings(&[("model_local", Value::from(7))]), &dir.path)
            .expect_err("refused");
        assert!(error.contains("takes a string, not a number"), "{error}");
    }
}
