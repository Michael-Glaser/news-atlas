//! Importing the model the reader chooses.
//!
//! The pinned file is what a fresh machine gets. A person who already has a `.gguf`, or
//! who wants a different one, is not asked to edit a constant: they run
//!
//! ```text
//! atlas-setup --import-model /absolute/path/to/model.gguf
//! atlas-setup --import-model https://example.org/model.gguf
//! ```
//!
//! and end up with the same three things a pinned install leaves behind: the file in
//! `models/`, a settings file that names it, and one line saying the app can be
//! restarted to load it.
//!
//! # The rules this module keeps
//!
//! * **A local file is copied, never moved and never linked.** The reader keeps their
//!   own file where they put it; a link would make `models/` depend on a folder that can
//!   be unmounted, renamed or emptied, and the failure would arrive at load time instead
//!   of here. [`std::fs::copy`] is the whole of it.
//! * **The first four bytes decide.** Every GGUF file starts with `GGUF`; a file that
//!   does not is not installed, whatever it is called. That check happens *before* the
//!   rename into place, so `models/` never holds something the app cannot read.
//! * **A download is the pinned download's machinery.** The body goes to
//!   `<name>.part`, resumes from its last byte, is judged by the same URL/address gate
//!   (`atlas_guard::Gated` and `atlas_net::approved_addresses_with`) and is written
//!   through the same chunk loop, so there is one transfer implementation in this crate
//!   and not two.
//! * **The settings file is written by `atlas-config`, not by this crate.** A value is
//!   validated, clamped and given its hardening note by the same code the CLI uses; a
//!   second implementation here would be a second answer to "which model does ATLAS
//!   serve".
//! * **A human starts it.** Nothing here runs at app run time, and nothing here listens:
//!   the copy is a file copy and the download is one outbound client connection.
//!
//! # What a reader is told
//!
//! One sentence to act on, and it names the exact file. The import ends with
//! [`next_line`] because "imported" without "what now" is a step that stops halfway.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use atlas_guard::Gated;
use atlas_net::{Lookup, SystemLookup};

use crate::model::{self, Fetched, Plan};
use crate::progress::SetupFile;

/// The four bytes every GGUF file starts with.
pub const MAGIC: &[u8; 4] = b"GGUF";

/// How many bytes the check reads: four, and no more of a 6.5 GB file.
pub const MAGIC_LEN: usize = 4;

/// What the reader named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An absolute path to a file they already have.
    Local(PathBuf),
    /// An https address that serves one.
    Url(String),
}

impl Source {
    /// The address or path, as it will be printed.
    pub fn text(&self) -> String {
        match self {
            Source::Local(path) => path.display().to_string(),
            Source::Url(url) => url.clone(),
        }
    }

    /// Where the bytes come from, in a word, for the progress file.
    pub fn word(&self) -> &'static str {
        match self {
            Source::Local(_) => "copying the model",
            Source::Url(_) => "downloading the model",
        }
    }
}

/// What a finished import leaves behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    /// The file in `models/` the app will load.
    pub path: PathBuf,
    /// Its size in bytes, as measured after it was put in place.
    pub bytes: u64,
    /// Its digest, so a person can pin it later.
    pub sha256: String,
    /// True when a local file was copied, false when it was downloaded.
    pub copied: bool,
    /// What `data/config.json` says `model_local` is now (the resolved value).
    pub configured: String,
}

// ---------------------------------------------------------------------------
// the sentences
// ---------------------------------------------------------------------------

/// How to use the command, for a person who typed it badly.
pub fn usage_line() -> String {
    "say which model to import: atlas-setup --import-model /absolute/path/model.gguf, or an \
     https address that serves a .gguf file"
        .to_string()
}

/// A relative path is refused: the working directory is not something this tool guesses.
pub fn relative_line(text: &str) -> String {
    format!(
        "{text} is a relative path, so ATLAS cannot tell which file you mean: type the whole \
         path (drag the file into the terminal window and the whole path appears), or give an \
         https address that serves it"
    )
}

/// A path with nothing at it.
pub fn missing_line(path: &Path) -> String {
    format!(
        "there is no file at {} - check the spelling, or give the https address the model comes \
         from. Nothing was imported",
        path.display()
    )
}

/// A file that is not a GGUF file, named by its path.
pub fn not_gguf_line(what: &str) -> String {
    format!(
        "{what} does not begin with the four letters GGUF, so it is not a model file ATLAS can \
         load. Nothing was imported"
    )
}

/// A local file that is already the model being served.
pub fn already_served_line(path: &Path) -> String {
    format!(
        "{} is already the model this copy of ATLAS serves, so there is nothing to copy",
        path.display()
    )
}

/// The address is not https.
pub fn not_https_line(text: &str) -> String {
    format!(
        "ATLAS downloads only from https addresses, and {text} is not one: use the https form of \
         the same address, or download the file yourself and import its path"
    )
}

/// An address that carries a user name or a password.
///
/// The decision is the URL gate's (`atlas_guard::urlsafe` refuses userinfo); this is the
/// same decision said in plain words, because "userinfo (user:pass@) is not allowed"
/// names a URL component, not a thing a reader did.
pub fn credential_line() -> String {
    "that address has a user name or a password inside it, and ATLAS accepts neither and stores \
     neither: remove the part before the @ sign and use the plain https address"
        .to_string()
}

/// What the settings write says when it worked.
pub fn served_line(config: &str) -> String {
    format!("settings: data/config.json now names {config} as the model ATLAS serves")
}

/// The last line of every successful import: the exact next step.
pub fn next_line() -> String {
    "restart ATLAS, or atlas start, to load it".to_string()
}

/// What the download says when the body was not a model.
pub fn wrong_body_line(url: &str) -> String {
    format!(
        "{url} did not deliver a GGUF file: the four bytes it starts with are not GGUF, so the \
         file it wrote was deleted and nothing was installed"
    )
}

// ---------------------------------------------------------------------------
// the pure halves
// ---------------------------------------------------------------------------

/// Do these bytes begin a GGUF file?
///
/// A file shorter than four bytes is not one: a truncated header cannot describe a model,
/// and accepting `GGU` would accept every file that starts with those letters.
pub fn starts_with_magic(head: &[u8]) -> bool {
    head.len() >= MAGIC_LEN && &head[..MAGIC_LEN] == MAGIC
}

/// Read just the first four bytes of a file and say whether they are the magic.
///
/// Four bytes, not the whole file: the check must be cheap enough to make before a copy
/// is kept and before a 6.5 GB download is called good.
pub fn has_magic(path: &Path) -> Result<bool, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("cannot open {}: {}", path.display(), error.kind()))?;
    let mut head = [0u8; MAGIC_LEN];
    let mut filled = 0usize;
    while filled < MAGIC_LEN {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) => {
                return Err(format!(
                    "cannot read {}: {}",
                    path.display(),
                    error.kind()
                ))
            }
        }
    }
    Ok(starts_with_magic(&head[..filled]))
}

/// What the reader typed: a path to copy, or an https address to download.
///
/// The URL itself is judged by [`Gated::verify`] — the same gate the app's own traffic
/// goes through, with the same rules about schemes, userinfo and invisible characters —
/// so this function adds no address rule of its own. What it adds is the shape question
/// (is this a path or an address at all) and the plain sentence a reader can act on.
pub fn parse_source(text: &str) -> Result<Source, String> {
    parse_source_with(text, false)
}

/// [`parse_source`] with the one exception the tests need: a plaintext loopback address,
/// which is the shape of a test server and of nothing else. Production passes `false`, so
/// the https rule and the URL gate are never relaxed in the binary.
fn parse_source_with(text: &str, loopback_test: bool) -> Result<Source, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(usage_line());
    }
    // A Windows-shaped path (`C:\...`) and a POSIX path are both paths; neither has a
    // scheme, and both are recognised by the platform's own rule.
    let looks_like_address = trimmed.to_ascii_lowercase().starts_with("https://")
        || trimmed.to_ascii_lowercase().starts_with("http://")
        || trimmed.contains("://");
    if looks_like_address {
        if !trimmed.to_ascii_lowercase().starts_with("https://") {
            // The test server is plain `http://127.0.0.1:<port>`. Nothing else is let
            // through, and this branch is not reachable from the binary.
            let loopback = trimmed.starts_with("http://127.0.0.1:")
                || trimmed.starts_with("http://localhost:");
            if !(loopback_test && loopback) {
                return Err(not_https_line(trimmed));
            }
            return Ok(Source::Url(trimmed.to_string()));
        }
        match Gated::verify(trimmed) {
            Ok(gated) => return Ok(Source::Url(gated.as_str().to_string())),
            Err(reason) => {
                // The gate's own reason decides; the credential case is the one worth
                // saying in plain words, because it is a thing the reader did.
                if reason.contains("userinfo") {
                    return Err(credential_line());
                }
                return Err(format!(
                    "that address cannot be used: {reason}"
                ));
            }
        }
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(relative_line(trimmed));
    }
    Ok(Source::Local(path))
}

/// The file name in `models/` for this source.
///
/// The last segment of the path or of the URL path, sanitised: no separator survives, so
/// nothing can be written outside `models/`, and the name always ends in `.gguf` because
/// that is what the app looks for.
pub fn file_name_for(source: &Source) -> String {
    let raw = match source {
        Source::Local(path) => path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        Source::Url(url) => {
            // The path part only: a query string or a fragment is not part of a name.
            let without_fragment = url.split('#').next().unwrap_or(url);
            let without_query = without_fragment.split('?').next().unwrap_or(without_fragment);
            let after_scheme = without_query
                .split("://")
                .nth(1)
                .unwrap_or(without_query);
            let path_part = after_scheme.split('/').skip(1).collect::<Vec<_>>().join("/");
            path_part
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string()
        }
    };
    let cleaned: String = raw
        .chars()
        .filter(|ch| !ch.is_control() && *ch != '/' && *ch != '\\')
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        return "model.gguf".to_string();
    }
    if cleaned.to_ascii_lowercase().ends_with(".gguf") {
        return cleaned;
    }
    format!("{cleaned}.gguf")
}

// ---------------------------------------------------------------------------
// the actions
// ---------------------------------------------------------------------------

/// Import the model the reader named.
///
/// `say` receives every line a person reads; `on_bytes` receives `(done, total)` while a
/// download runs, which is what `data/setup.json` is written from.
pub fn import(
    root: &Path,
    text: &str,
    say: &mut dyn FnMut(&str),
    on_bytes: &mut dyn FnMut(u64, u64),
) -> Result<Imported, String> {
    import_with(root, text, false, &SystemLookup, say, on_bytes)
}

/// [`import`] with the two exceptions a test needs: a plaintext loopback address for the
/// test server, and a scripted resolver. Neither is reachable from the binary.
pub fn import_with(
    root: &Path,
    text: &str,
    loopback_test: bool,
    lookup: &dyn Lookup,
    say: &mut dyn FnMut(&str),
    on_bytes: &mut dyn FnMut(u64, u64),
) -> Result<Imported, String> {
    let source = parse_source_with(text, loopback_test)?;
    let models = root.join("models");
    fs::create_dir_all(&models)
        .map_err(|error| format!("cannot create {}: {}", models.display(), error.kind()))?;

    let mut progress = SetupFile::at(root);
    let _ = progress.update("model", 0, 0, source.word());

    // Every failure from here on is an *attempt* that ended, so it leaves a verdict in
    // `data/setup.json`: a window showing a bar must not be left with a bar and no reason.
    let fetched = match &source {
        Source::Local(path) => copy_local(path, &models, say, &mut progress),
        Source::Url(url) => download(url, root, loopback_test, lookup, say, &mut progress, on_bytes),
    };
    let fetched = match fetched {
        Ok(landed) => landed,
        Err(line) => {
            let _ = progress.finish(false, &line);
            return Err(line);
        }
    };

    // The check that makes the install honest: those four bytes are what the model server
    // reads first, and a file without them is not installed at any size.
    let magic = has_magic(&fetched.path).unwrap_or(false);
    if !magic {
        // Nothing is left behind, so a second run starts from a clean folder.
        let _ = fs::remove_file(&fetched.path);
        let what = match &source {
            Source::Local(path) => path.display().to_string().clone(),
            Source::Url(url) => url.clone(),
        };
        let line = if fetched.copied {
            not_gguf_line(&what)
        } else {
            wrong_body_line(&what)
        };
        let _ = progress.finish(false, &line);
        return Err(line);
    }

    let (bytes, sha256) = model::verify_file(&fetched.path)?;
    say(&format!(
        "model: {} is in models/ ({} bytes)",
        fetched.name, bytes
    ));
    say(&format!("model: sha256 {sha256} - write this into models/model.json to pin it"));

    // The settings file, through `atlas-config`: the same validation, clamping and
    // hardening note the command line gets, so there is one answer to "which model".
    let configured = serve(root, &fetched.path, say)?;
    let _ = progress.finish(
        true,
        &format!("{} is the model ATLAS serves", fetched.name),
    );

    Ok(Imported {
        path: fetched.path,
        bytes,
        sha256,
        copied: fetched.copied,
        configured,
    })
}

/// One served model: a name, the file it landed in, and how it got there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Landed {
    name: String,
    path: PathBuf,
    copied: bool,
}

/// Copy a local file into `models/`, verifying it before it takes the name.
fn copy_local(
    source: &Path,
    models: &Path,
    say: &mut dyn FnMut(&str),
    progress: &mut SetupFile,
) -> Result<Landed, String> {
    let meta = fs::metadata(source).map_err(|_| missing_line(source))?;
    if !meta.is_file() {
        return Err(format!(
            "{} is not a file - name the .gguf itself, not the folder it is in. Nothing was \
             imported",
            source.display()
        ));
    }
    let name = file_name_for(&Source::Local(source.to_path_buf()));
    let target = models.join(&name);
    if same_file(source, &target) {
        return Err(already_served_line(source));
    }
    let staging = models.join(format!(".{name}.importing"));
    let _ = fs::remove_file(&staging);
    // A copy, deliberately: `fs::copy` reads and writes bytes, so the reader's own file
    // stays where it is and `models/` does not depend on a folder that can move.
    say(&format!(
        "model: copying {} into models/ ({} bytes)",
        source.display(),
        meta.len()
    ));
    fs::copy(source, &staging)
        .map_err(|error| format!("cannot copy {}: {}", source.display(), error.kind()))?;
    if !has_magic(&staging)? {
        let _ = fs::remove_file(&staging);
        return Err(not_gguf_line(&source.display().to_string()));
    }
    // Same directory, so this is a rename: `models/` never holds half a file, and the
    // name only ever appears complete.
    fs::rename(&staging, &target)
        .map_err(|error| format!("cannot put {} in place: {}", target.display(), error.kind()))?;
    let _ = progress.update("model", meta.len(), meta.len(), "model file is complete");
    Ok(Landed { name, path: target, copied: true })
}

/// Download an https address with the pinned download's machinery.
#[allow(clippy::too_many_arguments)]
fn download(
    url: &str,
    root: &Path,
    loopback_test: bool,
    lookup: &dyn Lookup,
    say: &mut dyn FnMut(&str),
    progress: &mut SetupFile,
    on_bytes: &mut dyn FnMut(u64, u64),
) -> Result<Landed, String> {
    let source = Source::Url(url.to_string());
    let name = file_name_for(&source);
    // No pin: the reader named an address, so the size the server reports and the digest
    // measured here are what this file has. That is stated, not hidden.
    let plan = Plan {
        name: name.clone(),
        repo: host_of(url),
        url: url.to_string(),
        bytes: 0,
        sha256: String::new(),
        licence: String::new(),
        redirect_hosts: model::redirect_hosts_for(root, url),
    };
    say(&format!(
        "model: downloading {url} into models/ - this one is not pinned, so its size and digest \
         are checked against what the server sends, and the digest is printed"
    ));
    let mut draw = |done: u64, total: u64| {
        let message = if done >= total && total > 0 {
            "model file is complete"
        } else {
            "downloading the model"
        };
        let _ = progress.tick("model", done, total, message);
        on_bytes(done, total);
    };
    let fetched: Fetched =
        model::fetch_url(&plan, root, loopback_test, lookup, say, &mut draw)?;
    Ok(Landed { name, path: fetched.path, copied: false })
}

/// Write the chosen file into `data/config.json` as `model_local`, through
/// `atlas-config`'s own save path.
///
/// That path validates and clamps every value, applies the model-path policy (absolute,
/// inside this project, no symlink out of it) and writes the file 0600 with a temp file
/// and a rename. A note it returns is printed here, because a library module does not
/// print.
pub fn serve(root: &Path, model: &Path, say: &mut dyn FnMut(&str)) -> Result<String, String> {
    let paths = atlas_config::Paths::at(root);
    let mut overrides = serde_json::Map::new();
    overrides.insert(
        "model_local".to_string(),
        serde_json::Value::String(model.display().to_string()),
    );
    let config = atlas_config::Config::save(&paths, &overrides)
        .map_err(|error| format!("the settings file could not be written: {error}"))?;
    for note in &config.notes {
        say(&format!("settings: {note}"));
    }
    if config.model_local.is_empty() {
        return Err(format!(
            "{} could not be named as the model: the settings policy refuses a path outside this \
             ATLAS folder. Copy it into models/ and name that path",
            model.display()
        ));
    }
    say(&served_line(&config.model_local));
    Ok(config.model_local)
}

/// The two paths are the same file (the reader pointed at the one already served).
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(left), Ok(right)) => left == right,
        _ => a == b,
    }
}

/// The host part of an address, for the record. Empty when there is none.
fn host_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or_default();
    after_scheme
        .split('/')
        .next()
        .unwrap_or_default()
        .split('@')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-import-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("rust")).expect("a crate root");
        fs::write(dir.join("rust").join("Cargo.toml"), "[workspace]\n").expect("a manifest");
        fs::create_dir_all(dir.join("models")).expect("models");
        dir
    }

    /// A tiny stand-in for a model: the magic, then a few bytes. Nothing here is a
    /// model; everything here is shaped like one, which is all this module checks.
    fn gguf_fixture(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut body = Vec::from(*MAGIC);
        body.extend_from_slice(b"a tiny fixture, not weights");
        fs::write(&path, &body).expect("a fixture");
        path
    }

    /// A one-answer HTTP server on 127.0.0.1:0, so this module's download test needs no
    /// network. Same shape as the one in `model.rs`; kept here so neither file's tests
    /// depend on the other's internals.
    struct Server {
        port: u16,
        stop: Arc<AtomicUsize>,
    }

    impl Server {
        fn start(body: Vec<u8>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let port = listener.local_addr().expect("an address").port();
            let stop = Arc::new(AtomicUsize::new(0));
            let closing = Arc::clone(&stop);
            std::thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                if closing.load(Ordering::SeqCst) == 1 {
                    return;
                }
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            });
            Server { port, stop }
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(1, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        }
    }

    // --- the pure halves ------------------------------------------------------------

    /// Four bytes, and only those four: a file that is short, or that spells the magic
    /// differently, is not a model.
    #[test]
    fn the_magic_is_the_first_four_bytes_and_nothing_else() {
        assert!(starts_with_magic(b"GGUF"));
        assert!(starts_with_magic(b"GGUF\x03\x00\x00\x00rest"));
        for wrong in [
            &b"GGU"[..],
            &b"gguf"[..],
            &b"\x00\x00\x00\x00"[..],
            &b"PK\x03\x04"[..],
            &b""[..],
        ] {
            assert!(!starts_with_magic(wrong), "{wrong:?}");
        }
    }

    /// The name never leaves `models/`, always ends in `.gguf`, and comes from the last
    /// segment of an address that may carry a query string.
    #[test]
    fn the_file_name_is_a_name_inside_models_and_ends_in_gguf() {
        let url = Source::Url(
            "https://example.org/a/b/model.gguf?download=true#frag".to_string(),
        );
        assert_eq!(file_name_for(&url), "model.gguf");

        let plain = Source::Url("https://example.org/weights/model".to_string());
        assert_eq!(file_name_for(&plain), "model.gguf");

        // A name with a separator in it cannot be a name here.
        let hostile = Source::Local(PathBuf::from("/tmp/../etc/passwd"));
        assert_eq!(file_name_for(&hostile), "passwd.gguf");

        let empty = Source::Url("https://example.org/".to_string());
        assert_eq!(file_name_for(&empty), "model.gguf");
    }

    /// A path is copied, an address is downloaded, and neither is guessed.
    #[test]
    fn what_the_reader_typed_is_read_as_a_path_or_an_address() {
        assert_eq!(
            parse_source("/tmp/model.gguf").expect("a path"),
            Source::Local(PathBuf::from("/tmp/model.gguf"))
        );
        assert_eq!(
            parse_source("https://example.org/m.gguf").expect("an address"),
            Source::Url("https://example.org/m.gguf".to_string())
        );
        // Relative paths are not guessed from the working directory.
        let error = parse_source("model.gguf").expect_err("a relative path");
        assert!(error.contains("relative path"), "{error}");
        assert!(error.contains("whole path"), "{error}");
        // http is refused before a socket exists.
        let error = parse_source("http://example.org/m.gguf").expect_err("plain http");
        assert!(error.contains("https"), "{error}");
        // Empty input names the shape of the command.
        assert!(parse_source("   ").expect_err("empty").contains("--import-model"));
    }

    /// An address that carries a password is refused, and the sentence says what to
    /// remove. The rule is the URL gate's (`atlas_guard`), which refuses userinfo; this
    /// test pins the plain words a reader gets on top of it.
    #[test]
    fn an_address_with_a_password_in_it_is_refused() {
        for url in [
            "https://user:hunter2@example.org/m.gguf",
            "https://token@example.org/m.gguf",
        ] {
            let error = parse_source(url).expect_err(url);
            assert_eq!(error, credential_line(), "{url}");
            assert!(error.contains("password"), "{error}");
            assert!(error.contains("@ sign"), "{error}");
            assert!(!error.contains("hunter2"), "the secret is not echoed: {error}");
            assert!(error.is_ascii(), "{error}");
        }
    }

    // --- the refusals that leave nothing behind --------------------------------------

    /// Four bytes that are not the magic: refused, and `models/` is as empty afterwards
    /// as it was before.
    #[test]
    fn a_file_that_is_not_a_gguf_is_refused_and_installs_nothing() {
        let root = scratch("not-gguf");
        let foreign = root.join("notes.gguf");
        fs::write(&foreign, b"NOPE and then a lot of bytes").expect("a non-model");
        let mut said: Vec<String> = Vec::new();
        let error = import(&root, &foreign.display().to_string(), &mut |line| said.push(line.to_string()), &mut |_, _| {})
            .expect_err("a file without the magic");
        assert!(error.contains("GGUF"), "{error}");
        assert!(error.contains("Nothing was imported"), "{error}");

        let entries: Vec<String> = fs::read_dir(root.join("models"))
            .expect("models")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.is_empty(), "models/ must stay empty: {entries:?}");
        assert!(
            said.iter().any(|line| line.contains("copying")),
            "the reader is told what it is about to do: {said:?}"
        );
        // And the settings file was not created for a model that was refused.
        assert!(!root.join("data").join("config.json").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    /// A path with nothing at it is a sentence, not a panic and not a half-copied file.
    #[test]
    fn a_path_with_nothing_at_it_says_so() {
        let root = scratch("missing");
        let error = import(
            &root,
            &root.join("nothing-here.gguf").display().to_string(),
            &mut |_| {},
            &mut |_, _| {},
        )
        .expect_err("a missing file");
        assert!(error.contains("there is no file at"), "{error}");
        assert!(error.contains("Nothing was imported"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    /// A folder is not a model, however it is named.
    #[test]
    fn a_folder_is_refused() {
        let root = scratch("folder");
        let folder = root.join("a-folder.gguf");
        fs::create_dir_all(&folder).expect("a folder");
        let error = import(&root, &folder.display().to_string(), &mut |_| {}, &mut |_, _| {})
            .expect_err("a folder");
        assert!(error.contains("is not a file"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    // --- the happy path -------------------------------------------------------------

    /// A real (tiny) GGUF fixture is copied into `models/`, and `data/config.json` names
    /// it: the file the app loads and the file the settings point at are the same file.
    #[test]
    fn a_gguf_fixture_is_copied_and_becomes_the_model_the_settings_name() {
        let root = scratch("copy");
        let elsewhere = root.join("downloads");
        fs::create_dir_all(&elsewhere).expect("a downloads folder");
        let source = gguf_fixture(&elsewhere, "chosen.gguf");
        let original = fs::read(&source).expect("read");

        let mut said: Vec<String> = Vec::new();
        let imported = import(
            &root,
            &source.display().to_string(),
            &mut |line| said.push(line.to_string()),
            &mut |_, _| {},
        )
        .expect("a copied fixture");

        assert!(imported.copied, "a local file is copied");
        assert_eq!(imported.path, root.join("models").join("chosen.gguf"));
        assert_eq!(fs::read(&imported.path).expect("read"), original);
        // The reader's own file is still where they put it: a copy, never a move.
        assert!(source.is_file(), "the source must survive the import");
        assert_eq!(imported.bytes, original.len() as u64);

        // The settings file, written by `atlas-config`, names exactly this file.
        let paths = atlas_config::Paths::at(&root);
        let stored = atlas_config::read_stored(&paths).expect("a settings file");
        let named = stored
            .get("model_local")
            .and_then(|value| value.as_str())
            .expect("model_local");
        assert_eq!(named, imported.path.display().to_string());
        assert_eq!(imported.configured, named);
        assert_eq!(imported.sha256, crate::sha::sha256(&original));

        // The last line is the next step, and the settings line names the file.
        assert!(said.iter().any(|line| line == &served_line(named)), "{said:?}");
        assert!(said.iter().any(|line| line.contains(&format!("{} bytes", original.len()))), "{said:?}");

        // 0600: the settings file is not world-readable (the mode `atlas-config` owns).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join("data").join("config.json"))
                .expect("the settings file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "data/config.json must be 0600, was {mode:o}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// The progress file is the same one the install writes: a finished import leaves the
    /// install's last shape (`ok`, `message`), and the byte counts of a transfer are in
    /// the tick shape the window draws a bar from.
    #[test]
    fn the_progress_file_is_reused_and_ends_finished() {
        let root = scratch("progress");
        let source = gguf_fixture(&root, "progress.gguf");
        import(&root, &source.display().to_string(), &mut |_| {}, &mut |_, _| {})
            .expect("a copied fixture");
        let text = fs::read_to_string(root.join("data").join("setup.json")).expect("setup.json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value.get("ok").and_then(|v| v.as_bool()), Some(true), "{text}");
        assert!(
            value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .contains("progress.gguf"),
            "the last line names the file: {text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A refused import writes a *false* verdict with its sentence, so a window showing
    /// progress is not left with a bar and no reason.
    #[test]
    fn a_refused_import_leaves_a_failure_verdict() {
        let root = scratch("progress-fail");
        let foreign = root.join("not-a-model.gguf");
        fs::write(&foreign, b"NOPE").expect("a non-model");
        import(&root, &foreign.display().to_string(), &mut |_| {}, &mut |_, _| {})
            .expect_err("a file without the magic");
        let text = fs::read_to_string(root.join("data").join("setup.json")).expect("setup.json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value.get("ok").and_then(|v| v.as_bool()), Some(false), "{text}");
        assert!(
            value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .contains("GGUF"),
            "{text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    // --- the download ---------------------------------------------------------------

    /// A body that is not a GGUF file leaves nothing behind: the partial machinery wrote
    /// it, the magic check refused it, and the file was deleted rather than renamed into
    /// `models/`.
    #[test]
    fn a_download_that_is_not_a_gguf_leaves_no_file() {
        let root = scratch("wrong-body");
        let server = Server::start(b"<html>404 not found</html>".to_vec());
        let url = server.url("/model.gguf");
        let mut said: Vec<String> = Vec::new();
        let error = import_with(
            &root,
            &url,
            true,
            &atlas_net::SystemLookup,
            &mut |line| said.push(line.to_string()),
            &mut |_, _| {},
        )
        .expect_err("a body without the magic");
        assert!(error.contains("did not deliver a GGUF file"), "{error}");
        assert!(error.contains("nothing was installed"), "{error}");
        assert!(error.is_ascii(), "{error}");
        let entries: Vec<String> = fs::read_dir(root.join("models"))
            .expect("models")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.is_empty(), "nothing may be kept: {entries:?}");
        assert!(!root.join("data").join("config.json").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    /// The download path is the pinned one: a body with the magic is kept, named from
    /// the address, and becomes the served model.
    #[test]
    fn a_download_with_the_magic_is_kept_and_named_from_the_address() {
        let root = scratch("right-body");
        let mut body = Vec::from(*MAGIC);
        body.extend_from_slice(b"a downloaded fixture");
        let server = Server::start(body.clone());
        let url = server.url("/weights/picked.gguf");
        let mut progress_seen: Vec<(u64, u64)> = Vec::new();
        let mut snapshots: Vec<serde_json::Value> = Vec::new();
        // Read `data/setup.json` from inside the transfer: this is the shape the window
        // draws its bar from, and it exists while the bytes are still arriving.
        let seen_root = root.clone();
        let imported = import_with(
            &root,
            &url,
            true,
            &atlas_net::SystemLookup,
            &mut |_| {},
            &mut |done, total| {
                progress_seen.push((done, total));
                if snapshots.is_empty() {
                    if let Ok(text) =
                        fs::read_to_string(seen_root.join("data").join("setup.json"))
                    {
                        if let Ok(value) = serde_json::from_str(&text) {
                            snapshots.push(value);
                        }
                    }
                }
            },
        )
        .expect("a downloaded fixture");
        let during = snapshots.first().expect("a snapshot while downloading");
        assert_eq!(during.get("phase").and_then(|v| v.as_str()), Some("model"), "{during}");
        assert!(during.get("done").is_some() && during.get("total").is_some(), "{during}");
        assert!(!imported.copied, "an address is downloaded");
        assert_eq!(imported.path, root.join("models").join("picked.gguf"));
        assert_eq!(fs::read(&imported.path).expect("read"), body);
        assert!(
            progress_seen.iter().any(|(done, total)| done == total && *total > 0),
            "{progress_seen:?}"
        );
        let paths = atlas_config::Paths::at(&root);
        let stored = atlas_config::read_stored(&paths).expect("settings");
        assert_eq!(
            stored.get("model_local").and_then(|v| v.as_str()),
            Some(imported.path.display().to_string().as_str())
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The reader is told what happens next, in the words the brief fixes, and every
    /// installer sentence is ASCII.
    #[test]
    fn the_last_line_names_the_next_step_in_ascii() {
        assert_eq!(next_line(), "restart ATLAS, or atlas start, to load it");
        for line in [
            usage_line(),
            relative_line("/x/y.gguf"),
            missing_line(Path::new("/x/y.gguf")),
            not_gguf_line("/x/y.gguf"),
            already_served_line(Path::new("/x/y.gguf")),
            not_https_line("ftp://example.org/m.gguf"),
            credential_line(),
            served_line("/root/models/m.gguf"),
            next_line(),
            wrong_body_line("https://example.org/m.gguf"),
        ] {
            assert!(line.is_ascii(), "not ASCII: {line}");
            assert!(!line.trim().is_empty());
        }
    }
}
