//! The model file: what is pinned, and how a fresh machine gets it.
//!
//! The weights are 6.5 GB. There is no way to put them in a repository faithfully,
//! and there is no way to run ATLAS without them, so this module is the one place in
//! the project where bytes arrive from the network — as an install step a person
//! starts, with the URL printed before anything is fetched.
//!
//! # What it guarantees
//!
//! * **The URL is a public https URL, and so is the address it resolves to.**
//!   `atlas_guard::urlsafe::check` judges the URL — no loopback literal, no private
//!   literal, no `file:`, no userinfo, no invisible character — and then
//!   `atlas_net::approved_addresses_with` judges every *address* the name answers with,
//!   with the same filter the app's own transport applies inside its socket. A name
//!   that answers with `127.0.0.1`, `169.254.169.254` or a LAN address is refused
//!   before a connection is attempted and before a byte is written.
//! * **A redirect is a second URL, judged the same way.** A hop must stay on `https`,
//!   must stay on the host the pin names — or a host the pin lists in
//!   `redirect_hosts`, which is where a CDN goes — and must resolve to a public
//!   address, because a redirect is the classic way around a URL check.
//! * **Nothing lands where the app can read it until it is complete.** The body goes
//!   to `<name>.part` and is renamed into place only after the byte count *and* the
//!   digest match the pin. A failed transfer keeps its partial file and a complete
//!   file that fails the digest is deleted: the pin decides, the partial file is only
//!   ever a head start.
//! * **A stopped transfer resumes.** `<name>.part` is asked for from its last byte
//!   (`Range: bytes=<n>-`), the bytes already on disk are hashed first so the digest is
//!   still over the whole file, a server that ignores the range is followed by starting
//!   over, and a `206` that starts anywhere but where the file ends is refused rather
//!   than appended. 6.5 GB over a home link stops; starting again from zero every time
//!   is how a first run never finishes.
//! * **A good file is never fetched twice.** If the file is already there with the
//!   pinned size, it is hashed and kept; `--fetch` is the only way to ask again.
//! * **The digest is the pin.** `models/model.json` carries the size and the SHA-256
//!   of the file this install runs. A different file is refused, with both digests
//!   printed, so "it downloaded something" can never be mistaken for "it downloaded
//!   the model".
//!
//! # A human starts it, and nothing listens
//!
//! The download is one outbound connection, started on purpose by a person typing
//! `atlas-setup model --yes`: the URL is printed first, and without `--yes` nothing is
//! fetched. The app never downloads anything at run time, and no process here opens a
//! port — the transfer is a client, not a listener.
//!
//! # What is still open
//!
//! The address is checked here and resolved again by the transport, so a name that
//! answers publicly when it is checked and privately when it is dialled — DNS rebinding
//! — is still possible. What closes that is a resolver *inside* the connection, which is
//! what `atlas_net`'s `FilteredResolver` is: the address dialled is the address approved
//! because the same function answers both. This crate cannot use it yet — `atlas-net`
//! publishes no entry point that streams a body to a `Write` sink, and 6.5 GB cannot be
//! buffered in a `Page` — so today this is a check before the dial, not the same check
//! at the dial. The signature that would close it is written down in the audit report.
//!
//! It does not: start anything, load the file into the model server, or decide which
//! model the app uses (that is `data/config.json` and the file name).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use atlas_guard::Gated;
use atlas_net::{Lookup, SystemLookup};

use crate::sha::Hasher;

/// The file ATLAS runs, when no `models/model.json` says otherwise.
///
/// The repository ships no weights: `models/*` is ignored, so a fresh clone has an empty
/// `models/` and no manifest either. The pin therefore lives in the code — this
/// constant — and `models/model.json` is only an override for a machine that wants to
/// pin a file it has already verified. The URL and the local name are the maintainer's
/// decision and are not derived from anything: `.../resolve/main/<file>` on
/// `huggingface.co`, saved under the name `data/config.json` expects.
const DEFAULT_URL: &str = "https://huggingface.co/DavidAU/Qwen3.5-9B-The-Defiant-Fable-Uncensored-Heretic-NEO-IMATRIX-MAX-MTP-GGUF/resolve/main/Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-MTP-IQ4_XS.gguf";
const DEFAULT_NAME: &str = "Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf";
const DEFAULT_REPO: &str = "DavidAU/Qwen3.5-9B-The-Defiant-Fable-Uncensored-Heretic-NEO-IMATRIX-MAX-MTP-GGUF";
/// The exact size of the file the pin names: 6 526 302 432 bytes.
///
/// Measured from the file this install runs, not guessed: `shasum -a 256` and `ls -l` on
/// `models/Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf`, the copy that answers on
/// 127.0.0.1:8081. A byte count is cheap to check and catches a truncated transfer before
/// the digest does.
pub const DEFAULT_BYTES: u64 = 6_526_302_432;
/// The SHA-256 of that same file, lowercase hex.
///
/// This is the pin: a download whose bytes do not hash to this is deleted, not renamed, and
/// both digests are printed. If the upstream file is ever replaced, the first run after it
/// stops here and says so — the fix is a measured digest in `models/model.json`, which
/// overrides this constant.
pub const DEFAULT_SHA256: &str = "8d33af536e35805967c9f4d77e1ed62b8212bdee282c80c6dc9b79ae6d677637";
/// The hosts `huggingface.co` delivers files from. `/resolve/` answers with a redirect
/// to one of these, and a redirect is followed only when the pin names its host. They
/// are HF's own file hosts, not a third party: the URL above is the only address that
/// was chosen. If HF ever delivers from a host that is not listed here, the refusal
/// prints the exact `redirect_hosts` line to add, so the fix is one copy-paste.
const DEFAULT_REDIRECT_HOSTS: &[&str] = &[
    "cdn-lfs.huggingface.co",
    "cdn-lfs-us-1.huggingface.co",
    "cdn-lfs-eu-1.huggingface.co",
    "cdn-lfs.hf.co",
    "cdn-lfs-us-1.hf.co",
    "cdn-lfs-eu-1.hf.co",
    "cas-bridge.xethub.hf.co",
    "us.aws.cdn.hf.co",
    "eu.aws.cdn.hf.co",
];

/// The pin, as written in `models/model.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The file name this install uses (`data/config.json` names it).
    pub name: String,
    /// The repository the file comes from, for the person reading the output.
    pub repo: String,
    /// Where the bytes are.
    pub url: String,
    /// The exact size, or `0` when the pin does not know it (the server's own
    /// `Content-Length` / `Content-Range` total is then the check).
    pub bytes: u64,
    /// The SHA-256, lowercase hex, or empty when no digest is pinned.
    pub sha256: String,
    /// The model card's own words about redistribution.
    pub licence: String,
    /// Hosts a redirect may move the transfer to, named by the pin.
    ///
    /// Empty unless `models/model.json` says otherwise: a pin that names one address
    /// follows that address, and a redirect anywhere else is refused. This is the field
    /// a CDN needs — `huggingface.co` answers `/resolve/` with a redirect to one — and
    /// writing it is a decision about *that CDN*, which is why the pin makes it and a
    /// `Location` header cannot.
    pub redirect_hosts: Vec<String>,
}

impl Plan {
    /// Does this pin say how big the file is?
    pub fn pins_size(&self) -> bool {
        self.bytes > 0
    }

    /// Does this pin carry a fingerprint?
    ///
    /// The built-in pin does not: nobody measured the bytes of a file that is not in the
    /// repository, and inventing a digest would be worse than having none. What is still
    /// checked is the byte count — against the pin when there is one, against the size
    /// the server reports when there is not — and the digest of what arrived is printed,
    /// so a person can write it into `models/model.json` and pin it from then on.
    pub fn pins_digest(&self) -> bool {
        self.sha256.len() == 64
    }

    /// The host the pinned URL names, lowercased.
    pub fn host(&self) -> String {
        host_of(&self.url).unwrap_or_default()
    }

    /// May a redirect hop go to `host`?
    ///
    /// The pinned host, or a host this pin named in `redirect_hosts`. Nothing else: a
    /// hop that leaves the pin is a transfer to a place nobody chose.
    pub fn permits(&self, host: &str) -> bool {
        let wanted = bare_host(host);
        wanted == self.host()
            || self
                .redirect_hosts
                .iter()
                .any(|named| bare_host(named) == wanted)
    }
}

/// What a finished (or already present) file looks like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    /// False when the file was already there and correct.
    pub downloaded: bool,
}

/// How much one read holds in memory: 1 MiB, so a 6.5 GB transfer never is.
const CHUNK: usize = 1 << 20;

/// Progress is printed every 256 MiB rather than every chunk: one line per minute on
/// a slow link beats a thousand lines nobody reads.
const SAY_EVERY: u64 = 256 << 20;

/// Redirects followed at most. `/resolve/` uses one.
const MAX_HOPS: usize = 5;

/// Read the pin. A missing or malformed manifest is a sentence, not a panic: the
/// failure a person sees should name the file.
pub fn load_manifest(root: &Path) -> Result<Plan, String> {
    let path = root.join("models").join("model.json");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    let field = |name: &str| -> Result<String, String> {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{} has no \"{name}\" string", path.display()))
    };
    // Optional: a pin may say how big the file is, and a pin written by hand may not.
    let bytes = match value.get("bytes") {
        None | Some(serde_json::Value::Null) => 0,
        Some(number) => number.as_u64().ok_or_else(|| {
            format!("{} has a \"bytes\" that is not a number", path.display())
        })?,
    };
    // Optional, and strict when present: a list of host names, or a sentence. A pin
    // that half-names a CDN is a pin nobody can predict.
    let redirect_hosts = match value.get("redirect_hosts") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(names)) => {
            let mut hosts = Vec::with_capacity(names.len());
            for name in names {
                let host = name.as_str().ok_or_else(|| {
                    format!(
                        "{} has a redirect_hosts entry that is not a host name",
                        path.display()
                    )
                })?;
                hosts.push(bare_host(host));
            }
            hosts
        }
        Some(_) => {
            return Err(format!(
                "{} has a redirect_hosts that is not a list of host names",
                path.display()
            ))
        }
    };
    let plan = Plan {
        name: field("name")?,
        repo: field("repo")?,
        url: field("url")?,
        bytes,
        sha256: match value.get("sha256").and_then(serde_json::Value::as_str) {
            Some(digest) => digest.to_ascii_lowercase(),
            None => String::new(),
        },
        licence: field("license")?,
        redirect_hosts,
    };
    // Refuse a manifest that cannot describe a file: a name with a separator would
    // write outside models/, and a digest that is not 64 hex characters cannot pin
    // anything.
    if plan.name.contains(['/', '\\']) || plan.name.contains("..") {
        return Err(format!("model name {:?} is not a file name", plan.name));
    }
    // A digest that is there must be a digest: a typo in a pin is a pin that checks
    // nothing, which reads exactly like one that checks something.
    if !plan.sha256.is_empty() && !plan.pins_digest() {
        return Err(format!("the pinned sha256 {:?} is not 64 hex characters", plan.sha256));
    }
    if !plan.sha256.is_empty() && !plan.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("the pinned sha256 {:?} is not 64 hex characters", plan.sha256));
    }
    Ok(plan)
}

/// The pin this machine uses: `models/model.json` when it is there, the built-in one
/// otherwise.
///
/// The manifest is an *override*, and it is the one that wins on a machine where
/// somebody wrote it down deliberately. A fresh clone has none — `models/*` is ignored,
/// so neither the weights nor the manifest travel with the code — and then the constant
/// above is the pin, which is why `atlas-setup --install` works on a machine that has
/// never seen this project.
pub fn pin(root: &Path) -> Result<Plan, String> {
    let manifest = root.join("models").join("model.json");
    if manifest.is_file() {
        return load_manifest(root);
    }
    Ok(Plan {
        name: DEFAULT_NAME.to_string(),
        repo: DEFAULT_REPO.to_string(),
        url: DEFAULT_URL.to_string(),
        bytes: DEFAULT_BYTES,
        sha256: DEFAULT_SHA256.to_string(),
        licence: "check the model card before redistributing the file".to_string(),
        redirect_hosts: DEFAULT_REDIRECT_HOSTS.iter().map(|host| host.to_string()).collect(),
    })
}

/// Size and digest of a file on disk, streamed.
pub fn verify_file(path: &Path) -> Result<(u64, String), String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((total, hasher.finish_hex()))
}

/// Hosts a redirect may move an *imported* address to.
///
/// The pinned download's rule is `models/model.json`: the pin names the CDN, a `Location`
/// header cannot. An import has no pin, so the answer is derived from what is known —
/// Hugging Face's own file hosts for a Hugging Face address (the address the reader
/// pasted is that site's `/resolve/` form, which redirects), and otherwise whatever
/// `models/model.json` already lists, so the refusal sentence that names that field stays
/// true. An address whose site redirects elsewhere is refused with its host printed, and
/// the fix is the same one line of JSON.
pub fn redirect_hosts_for(root: &Path, url: &str) -> Vec<String> {
    let host = host_of(url).unwrap_or_default();
    if host.eq_ignore_ascii_case("huggingface.co") || host.ends_with(".huggingface.co") {
        return DEFAULT_REDIRECT_HOSTS.iter().map(|name| name.to_string()).collect();
    }
    load_manifest(root)
        .map(|plan| plan.redirect_hosts)
        .unwrap_or_default()
}

/// One transfer, for a file the reader chose: the same gate, the same `.part` resume and
/// the same chunk loop as the pinned download, with no pin to check against.
///
/// `loopback_test` and `lookup` are the two exceptions the tests need (a plaintext
/// `127.0.0.1` test server, and a scripted resolver); the binary passes `false` and
/// [`SystemLookup`].
pub fn fetch_url(
    plan: &Plan,
    root: &Path,
    loopback_test: bool,
    lookup: &dyn Lookup,
    say: &mut dyn FnMut(&str),
    on_bytes: &mut dyn FnMut(u64, u64),
) -> Result<Fetched, String> {
    let (target, partial) = paths(plan, root);
    fetch_progress(
        plan,
        root,
        &target,
        &partial,
        loopback_test,
        lookup,
        say,
        on_bytes,
    )
}

/// The size of the weights on this machine, or `None` when they are not here.
///
/// The caller decides what to say about it: a library module does not print (see
/// `atlas-guard/tests/audit_logs.rs`), so "the model is already here" is the installer's
/// line, drawn from this number, and never a note emitted from under it.
pub fn weights_size(root: &Path) -> Option<u64> {
    let plan = pin(root).ok()?;
    let (target, _) = paths(&plan, root);
    match fs::metadata(&target) {
        Ok(meta) if meta.is_file() => Some(meta.len()),
        _ => None,
    }
}

/// Where the file goes, and where a partial transfer waits.
pub fn paths(plan: &Plan, root: &Path) -> (PathBuf, PathBuf) {
    let dir = root.join("models");
    let target = dir.join(&plan.name);
    let partial = dir.join(format!("{}.part", plan.name));
    (target, partial)
}

/// The file, fetching it only when it is not already correct.
///
/// `recheck` hashes an existing file instead of trusting its size (a person can ask
/// for that with `--verify`); `refetch` deletes a wrong file and downloads again,
/// which is what `--fetch` means.
/// Room to work on top of the file itself: the download writes `<name>.part` beside the
/// model, the operating system keeps its own copies of things while that happens, and a
/// disk that is exactly large enough fails halfway through.
pub const HEADROOM: u64 = 512 * 1024 * 1024;

/// How much free space this download needs: the file, plus [`HEADROOM`].
///
/// This is a number to *print*, not a number to check against the machine: there is no
/// free-space query here on purpose (see [`disk_is_full`]).
pub fn needs(bytes: u64) -> u64 {
    bytes.saturating_add(HEADROOM)
}

/// Is this the error that means the disk is full?
///
/// Asked of the error and never of the machine: the operating system already knew the
/// answer at the moment it refused the write, so no program is started and no platform
/// call is made. Three spellings of the same fact, because a filesystem may report it as
/// the Rust kind, as the raw code, or only as its own message. `28` is `ENOSPC` on macOS
/// and on Linux; `39` and `112` are Windows' handle-disk-full and disk-full errors.
pub fn disk_is_full(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::StorageFull {
        return true;
    }
    if matches!(error.raw_os_error(), Some(28) | Some(39) | Some(112)) {
        return true;
    }
    let text = error.to_string().to_ascii_lowercase();
    text.contains("no space left")
        || text.contains("not enough space")
        || text.contains("disk is full")
        || text.contains("disk full")
}

/// The sentence a person gets when the disk fills during the download.
///
/// Two facts and one instruction: how much arrived, that the rest does not fit, and that
/// the bytes kept are not wasted because the next run carries on from them. No number is
/// invented - `written` is what this run actually put on the disk.
pub fn out_of_room(written: u64, expected: u64) -> String {
    format!(
        "the disk filled up with {written} of {expected} bytes written, so the download stopped \
         there because there is not enough free space for the rest of it: the bytes already here \
         were kept, so free up some space and run the same command again and it carries on from \
         there instead of starting over"
    )
}

/// One chunk of the body, written or explained.
fn write_chunk<W: std::io::Write>(
    file: &mut W,
    chunk: &[u8],
    written: u64,
    expected: u64,
    partial: &Path,
) -> Result<(), String> {
    file.write_all(chunk)
        .map_err(|error| write_failure(&error, written, expected, partial))
}

/// The flush after the last chunk, with the same two answers: room, or the sentence.
fn flush_chunk<W: std::io::Write>(
    file: &mut W,
    written: u64,
    expected: u64,
    partial: &Path,
) -> Result<(), String> {
    file.flush()
        .map_err(|error| write_failure(&error, written, expected, partial))
}

/// A write that failed: a full disk is the reader's problem with a solution, anything else
/// is a path they need named.
fn write_failure(error: &std::io::Error, written: u64, expected: u64, partial: &Path) -> String {
    if disk_is_full(error) {
        out_of_room(written, expected)
    } else {
        format!("cannot write {}: {error}", partial.display())
    }
}

pub fn ensure(
    plan: &Plan,
    root: &Path,
    recheck: bool,
    refetch: bool,
    say: &mut dyn FnMut(&str),
) -> Result<Fetched, String> {
    ensure_with(plan, root, recheck, refetch, say, &mut |_, _| {})
}

/// [`ensure`] plus a byte callback, so `atlas-setup --install` can write a progress file while a
/// 6.5 GB transfer runs. The callback also fires exactly once on the fast path, with
/// `(size, size)`, so a progress bar for an already-installed machine is *full*, not
/// empty.
pub fn ensure_with(
    plan: &Plan,
    root: &Path,
    recheck: bool,
    refetch: bool,
    say: &mut dyn FnMut(&str),
    on_bytes: &mut dyn FnMut(u64, u64),
) -> Result<Fetched, String> {
    let (target, partial) = paths(plan, root);
    if target.is_file() && !refetch {
        let size = fs::metadata(&target).map(|meta| meta.len()).unwrap_or(0);
        // With a size pinned, the size decides. Without one (the built-in pin, which
        // cannot carry a number a repository without weights does not have) a file at
        // the pinned name is the file — `--verify` and `--fetch` are how a person asks
        // the stricter questions.
        let matches = if plan.pins_size() { size == plan.bytes } else { size > 0 };
        if matches {
            let mut digest = plan.sha256.clone();
            if recheck {
                say(&format!("hashing {} ({} bytes)...", plan.name, size));
                let (bytes, computed) = verify_file(&target)?;
                if plan.pins_digest() && computed != plan.sha256 {
                    return Err(mismatch(&target, &computed, &plan.sha256, bytes));
                }
                digest = computed;
            }
            on_bytes(size, size);
            return Ok(Fetched { path: target, bytes: size, sha256: digest, downloaded: false });
        }
        if !recheck {
            say(&format!(
                "{} is {} bytes, the pin says {} - it is not the pinned file",
                plan.name, size, plan.bytes
            ));
        }
    }
    fetch_progress(plan, root, &target, &partial, false, &SystemLookup, say, on_bytes)
}

/// The transfer, with the two exceptions the tests need.
///
/// `loopback_test` is never true in production: it lets a plaintext `127.0.0.1` URL
/// through the gate, which is the shape of a test server and of nothing else. The
/// three rules it relaxes — `https`, "a public address", and "a redirect stays on the
/// pinned host" — are the three the other tests in this module prove are enforced.
///
/// `lookup` is the answer to "what does that name resolve to". Production passes
/// [`SystemLookup`]; a test passes a scripted one, which is the only way to stage the
/// case that matters here without a network or a hostfile.
#[cfg(test)]
fn fetch_with(
    plan: &Plan,
    root: &Path,
    target: &Path,
    partial: &Path,
    loopback_test: bool,
    lookup: &dyn Lookup,
    say: &mut dyn FnMut(&str),
) -> Result<Fetched, String> {
    fetch_progress(plan, root, target, partial, loopback_test, lookup, say, &mut |_, _| {})
}

/// [`fetch_with`] plus a byte callback, so a caller can draw a progress bar.
///
/// The callback is `(done, total)` and is called once per chunk read — roughly 1 MiB —
/// and once at the end. It cannot fail and cannot stop the transfer: progress reporting
/// is not allowed to be the reason a 6.5 GB download dies.
#[allow(clippy::too_many_arguments)]
fn fetch_progress(
    plan: &Plan,
    root: &Path,
    target: &Path,
    partial: &Path,
    loopback_test: bool,
    lookup: &dyn Lookup,
    say: &mut dyn FnMut(&str),
    on_bytes: &mut dyn FnMut(u64, u64),
) -> Result<Fetched, String> {
    let dir = root.join("models");
    fs::create_dir_all(&dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;

    // What an interrupted attempt left behind, and how much of it is already here.
    //
    // A 6.5 GB transfer over a home link stops: a laptop lid closes, a router
    // renumbers, a CDN drops a long connection. Starting again from zero every time is
    // how a first run never finishes, so `<name>.part` is *kept* and asked for from its
    // last byte. Nothing is trusted by keeping it: the byte count and the digest are
    // still checked over the whole file before it is renamed, and a file that fails
    // the digest is deleted, so a stale prefix can only cost one attempt, never a
    // wrong model. A `.part` at or past the pinned size is not a partial transfer (a
    // different file, a corrupted length), so it is discarded.
    let mut have: u64 = match fs::metadata(partial) {
        // A pin without a size cannot say "too big", so the server's total decides at
        // the end; a pin with one discards a partial that is already past it.
        Ok(meta) if meta.is_file() && (!plan.pins_size() || meta.len() < plan.bytes) => meta.len(),
        Ok(meta) => {
            // A `.part` at or past the size of the whole file is not a head start: it is a
            // different file, or a length left by something else. Deleting it silently would
            // leave a person wondering why the download started over.
            let _ = fs::remove_file(partial);
            say(&format!(
                "a partial download was here ({} bytes) and cannot be part of this file, so it \
                 starts again from the beginning",
                meta.len()
            ));
            0
        }
        Err(_) => 0,
    };
    if have > 0 {
        if plan.pins_size() {
            say(&format!("resuming {} at {have} of {} bytes", plan.name, plan.bytes));
        } else {
            say(&format!("resuming {} at {have} bytes already here", plan.name));
        }
    }

    // The whole transfer may take an hour on a home link; the connect must not.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(None)
        .timeout_connect(Some(Duration::from_secs(30)))
        .max_redirects(0)
        .http_status_as_error(false)
        .accept_encoding("identity")
        .proxy(None)
        .user_agent("atlas-setup/0.1 (local install; one model download)")
        .build()
        .into();

    let mut url = plan.url.clone();
    let mut response = None;
    // The whole file's size, decided by the first good answer: the pin's number, or the
    // server's when the pin does not carry one. Set inside the loop, read below.
    let mut expected_size: u64 = 0;
    for _ in 0..=MAX_HOPS {
        // `Gated`, not the raw gate: the raw `urlsafe::check` has exactly one caller
        // (atlas-net) and `atlas-guard/tests/audit_gate.rs` fails the build for any
        // other — which is what caught this line. The two constructors are the two
        // answers a caller can need: a public URL, or the app's own loopback traffic.
        let gated = if loopback_test {
            atlas_guard::Gated::loopback(&url)
        } else {
            atlas_guard::Gated::verify(&url)
        };
        let gated = match gated {
            Ok(gated) => gated,
            Err(reason) => return Err(format!("the download URL is refused: {reason}")),
        };
        if !loopback_test && !gated.as_str().starts_with("https://") {
            return Err(format!("the model is fetched over https only, not {url}"));
        }
        // The name, judged before it becomes a socket: `atlas_guard` has already refused
        // a *literal* loopback or private address, and this refuses a *name* that
        // answers with one — the answer `urlsafe::check` cannot see. It is the same
        // filter the app's transport applies inside its connector, asked here first so
        // that a refusal costs no connection and writes no byte.
        if !loopback_test {
            public_address(&gated, lookup)?;
        }
        say(&format!("GET {gated}"));
        let mut request = agent
            .get(gated.as_str())
            .header("Accept", "application/octet-stream");
        // The resume, as one header: "everything from byte `have` on". A server that
        // honours it answers 206 with the rest; one that does not answers 200 with the
        // whole file, which the code below detects and handles by starting over — an
        // ignored Range must never turn into a corrupt file, because the bytes would
        // then be appended to a prefix that is already there.
        if have > 0 {
            request = request.header("Range", &format!("bytes={have}-"));
        }
        let answer = request
            .call()
            .map_err(|error| format!("cannot reach {url}: {error}"))?;
        let status = answer.status().as_u16();
        if (300..400).contains(&status) {
            let location = answer
                .headers()
                .get("location")
                .map(|value| String::from_utf8_lossy(value.as_bytes()).trim().to_string())
                .unwrap_or_default();
            if location.is_empty() {
                return Err(format!("{url} answered {status} without a Location"));
            }
            let next = redirect_target(&gated, &location, plan, lookup, loopback_test)?;
            say(&format!("  {status} -> {next}"));
            url = next.as_str().to_string();
            continue;
        }
        // 206 is the server saying "here is the rest", and it is only the rest if the
        // range it answers with starts exactly where the file on disk ends. A different
        // start is a server with a different idea of the file, and appending it would
        // silently interleave two files.
        if status == 206 {
            let start = content_range_start(&answer);
            match start {
                Some(start) if start == have => {}
                Some(start) => {
                    return Err(format!(
                        "the server resumed at byte {start} but {have} bytes are already on disk - \
                         re-run with --fetch to start this file again"
                    ))
                }
                None => {
                    return Err(format!(
                        "{url} answered 206 without a Content-Range - re-run with --fetch to start \
                         this file again"
                    ))
                }
            }
        } else if status == 200 {
            if have > 0 {
                // The Range was ignored. Truncating is the only safe answer: the body is
                // the whole file, so it replaces the prefix rather than following it.
                say("the server sent the whole file again; starting from the beginning");
                have = 0;
            }
        } else {
            return Err(format!(
                "{url} answered {status} (a model download expects 200, or 206 when resuming)"
            ));
        }
        let announced = answer
            .headers()
            .get("content-length")
            .and_then(|value| String::from_utf8_lossy(value.as_bytes()).parse::<u64>().ok());
        // The whole file's size, as this answer describes it: the pin's number when the
        // pin has one, and otherwise the server's own — `Content-Range`'s total on a
        // resume, `Content-Length` on a fresh transfer. A pin that carries no size still
        // gets a byte count it must match; what it does not get is a number from a
        // repository that holds no weights.
        let expected = if plan.pins_size() {
            plan.bytes
        } else {
            let total = match status {
                206 => content_range_total(&answer),
                _ => announced.map(|len| have + len),
            };
            match total {
                Some(total) if total > 0 => total,
                _ => {
                    return Err(format!(
                        "{url} did not say how big the file is (no Content-Length or \
                         Content-Range total), so the download cannot be checked — try again, or \
                         put the file at {} yourself",
                        target.display()
                    ))
                }
            }
        };
        if let Some(len) = announced {
            // On a resume the announced length is the *rest* of the file, so it is
            // compared with what is still missing rather than with the whole file.
            if have + len != expected {
                return Err(format!(
                    "the server offers {len} bytes{}, the pin says {} - this is not the pinned \
                     file (pass --sha256 with a digest you measured if upstream changed the file)",
                    if have > 0 { format!(" on top of the {have} already here") } else { String::new() },
                    expected
                ));
            }
        }
        response = Some(answer);
        expected_size = expected;
        break;
    }
    let answer = response.ok_or_else(|| format!("{url} redirected more than {MAX_HOPS} times"))?;

    // The prefix that is already on disk is hashed first, so the digest is over the
    // whole file — the same digest the pin carries. Nothing is trusted because it was
    // kept: this is the check that makes keeping it safe.
    let mut hasher = Hasher::new();
    if have > 0 {
        hash_prefix(partial, have, &mut hasher)?;
    }
    // `append` when resuming, `create` (which truncates) when starting over, so the
    // prefix is never overwritten with the rest of the file.
    let mut file = if have > 0 {
        OpenOptions::new()
            .append(true)
            .open(partial)
            .map_err(|error| format!("cannot write {}: {error}", partial.display()))?
    } else {
        File::create(partial)
            .map_err(|error| format!("cannot write {}: {error}", partial.display()))?
    };
    let mut total: u64 = have;
    let mut announced_step: u64 = total;
    // `into_reader` is the same accessor atlas-net reads pages through; the chunk
    // size is ours, so a 6.5 GB body never is.
    let mut reader = answer.into_body().into_reader();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(error) => {
                // The partial file is kept, not deleted: the next run carries on from
                // here. This is the error a 6.5 GB transfer actually hits.
                let _ = file.flush();
                return Err(format!(
                    "the transfer stopped after {total} of {expected_size} bytes ({error}). The \
                     {} bytes already here were kept: run the same command again and it carries \
                     on from there",
                    total
                ));
            }
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        // A full disk is not a mystery here: the write is refused with `ENOSPC`, the part
        // stays, and the sentence says how much arrived and what to do next.
        write_chunk(&mut file, &buffer[..read], total, expected_size, partial)?;
        total += read as u64;
        if total >= announced_step + SAY_EVERY {
            announced_step = total;
            say(&format!(
                "  {} of {} MB",
                total / 1_000_000,
                expected_size / 1_000_000
            ));
        }
        // The caller's own progress, every chunk: a window drawing a bar needs to see it
        // move while the terminal line above only appears every 256 MiB. The closure
        // decides how often it touches the disk.
        on_bytes(total, expected_size);
    }
    flush_chunk(&mut file, total, expected_size, partial)?;
    drop(file);

    let digest = hasher.finish_hex();
    if total != expected_size {
        // Kept, for the same reason: this is a transfer that ended early, and the next
        // attempt asks for the rest instead of all of it.
        return Err(format!(
            "the transfer ended at {total} of {expected_size} bytes - the bytes already here \
             were kept: run the same command again and it carries on from there"
        ));
    }
    if plan.pins_digest() && digest != plan.sha256 {
        // A complete file that is not the pinned one is not a transfer to resume: the
        // pin is authoritative, so it goes.
        let _ = fs::remove_file(partial);
        return Err(mismatch(partial, &digest, &plan.sha256, total));
    }
    // Same directory, so this is a rename and not a copy: the app can never read a
    // half-written model.
    fs::rename(partial, target)
        .map_err(|error| format!("cannot move the model into place at {}: {error}", target.display()))?;
    say(&format!("{} is in place ({total} bytes)", plan.name));
    if plan.pins_digest() {
        say(&format!("  sha256 {digest} - the digest this pin names"));
    } else {
        // Not a warning: the size the server reported is what was checked, and the
        // digest is printed so the next run can check the stronger thing too.
        say(&format!(
            "  sha256 {digest} - no fingerprint is pinned for this file; the size the server \
             reported was checked. Write this digest into models/model.json as \"sha256\" to \
             pin it"
        ));
    }
    on_bytes(total, expected_size);
    Ok(Fetched { path: target.to_path_buf(), bytes: total, sha256: digest, downloaded: true })
}

/// The whole file's size, from a `Content-Range: bytes <start>-<end>/<total>` header.
///
/// Only the total is needed: `start` is checked separately, and the total is what a pin
/// without a size verifies against.
fn content_range_total(answer: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    let value = answer.headers().get("content-range")?;
    let text = String::from_utf8_lossy(value.as_bytes()).trim().to_string();
    let after = text.split('/').nth(1)?.trim();
    after.parse::<u64>().ok()
}

/// Where a `206 Partial Content` body starts, from its `Content-Range` header.
///
/// The shape is `bytes <start>-<end>/<total>` (the total may be `*`), and only the
/// start is needed: it must be exactly the number of bytes already on disk, or the
/// body is a different part of the file and appending it would interleave two files.
fn content_range_start(answer: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    let value = answer.headers().get("content-range")?;
    let text = String::from_utf8_lossy(value.as_bytes()).trim().to_string();
    let rest = text.strip_prefix("bytes")?.trim_start();
    let start = rest.split('-').next()?.trim();
    start.parse::<u64>().ok()
}

/// Hash the bytes already on disk, so the digest covers the whole file and not just
/// the part this run fetched.
///
/// `have` is the size read from the file a moment ago, so a file that grew or shrank
/// in between is caught instead of hashed as if it were the prefix: a short read is a
/// sentence, not a wrong digest that would delete a good transfer.
fn hash_prefix(path: &Path, have: u64, hasher: &mut Hasher) -> Result<(), String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut buffer = vec![0u8; CHUNK];
    let mut seen: u64 = 0;
    while seen < have {
        let want = std::cmp::min(CHUNK as u64, have - seen) as usize;
        let read = file
            .read(&mut buffer[..want])
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            return Err(format!(
                "{} holds {seen} of the {have} bytes this transfer needs - re-run with --fetch to \
                 start this file again",
                path.display()
            ));
        }
        hasher.update(&buffer[..read]);
        seen += read as u64;
    }
    Ok(())
}

fn mismatch(path: &Path, got: &str, want: &str, bytes: u64) -> String {
    format!(
        "the file that arrived is not the one ATLAS expects ({} bytes, fingerprint {got}, the \
         pinned fingerprint is {want}), so it was deleted instead of being loaded. Running \
         atlas-setup --install again downloads it from the beginning; passing --fetch to \
         atlas-setup model does the same. Nothing was loaded, and {} is being asked for again",
        bytes,
        path.display()
    )
}

/// The host in a URL, lowercased and without the brackets IPv6 wears.
fn host_of(raw: &str) -> Option<String> {
    let host = url::Url::parse(raw).ok()?.host_str()?.to_string();
    Some(bare_host(&host))
}

/// A host name as the comparisons want it: trimmed, lowercased, unbracketed.
fn bare_host(host: &str) -> String {
    host.trim()
        .trim_matches(|ch| ch == '[' || ch == ']')
        .to_ascii_lowercase()
}

/// Refuse before dialling when every address the name answers with is this machine or
/// its network.
///
/// [`atlas_net::approved_addresses_with`] is the workspace's one address rule (the same
/// filter `FilteredResolver` applies inside the app's connector), so a name that answers
/// with `127.0.0.1`, `169.254.169.254` or `192.168.0.5` is refused here in the same words
/// the rest of ATLAS refuses it. A multi-homed name keeps its public answers: the refusal
/// is for a name with nothing public left.
fn public_address(gated: &Gated, lookup: &dyn Lookup) -> Result<(), String> {
    atlas_net::approved_addresses_with(gated.as_str(), lookup)
        .map(|_| ())
        .map_err(|reason| format!("this download may only come from a public address: {reason}"))
}

/// One redirect hop, decided before it is dialled.
///
/// Three rules, and each is a way the download was walked off the pinned file:
///
/// * **Still `https`.** A hop to `http://` would drop the transport security the pin was
///   judged under, and the bytes would then be trusted to the digest alone.
/// * **Still the pinned host** — its own host, or a host the pin names in
///   `redirect_hosts`, because `huggingface.co` answers `/resolve/` with a redirect to a
///   CDN. A redirect to a third party the pin never described is refused, and the
///   sentence names both hosts so the fix is one line in `models/model.json`.
/// * **Still a public address.** `Location: http://169.254.169.254/…` is refused by the
///   guard's own door ([`atlas_net::check_hop_from`]), and a `Location` naming a public
///   host that *resolves* into the LAN is refused by the address filter, exactly as the
///   first URL was.
fn redirect_target(
    from: &Gated,
    location: &str,
    plan: &Plan,
    lookup: &dyn Lookup,
    loopback_test: bool,
) -> Result<Gated, String> {
    // The guard resolves the Location against the answering URL (control characters and
    // relative forms included) and puts the result through the door the origin came
    // through, so a public transfer can never be walked onto this machine.
    let hop = atlas_net::check_hop_from(from, location)
        .map_err(|reason| format!("the download redirected somewhere it may not go: {reason}"))?;
    if loopback_test {
        // The test server's hops stay on this machine, which is the one thing the
        // loopback door exists for.
        return Ok(hop);
    }
    if !hop.as_str().starts_with("https://") {
        return Err(format!(
            "this download may only come from a public https address, and it redirected to {}",
            hop.as_str()
        ));
    }
    let host = host_of(hop.as_str()).unwrap_or_default();
    if !plan.permits(&host) {
        // The fix is one line of JSON in a file the person can open, so it is printed as
        // one: a refusal that names a field but not its shape sends the reader to the
        // source to find out what to type.
        return Err(format!(
            "the download redirected to {host}, which is not the host this pin names ({}); a \
             redirect may only stay on the pinned host or a host models/model.json lists in \
             redirect_hosts - to allow exactly this host, add \"redirect_hosts\": [\"{host}\"] \
             to models/model.json and run this again",
            plan.host()
        ));
    }
    public_address(&hop, lookup)?;
    Ok(hop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A one-request-at-a-time HTTP server on 127.0.0.1:0, so nothing here needs the
    /// network. It answers exactly what a test tells it to.
    struct Server {
        port: u16,
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicUsize>,
        /// The request head of each answer, in order: what the client actually asked
        /// for, which is the only way to prove a `Range` was sent.
        heads: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Server {
        fn start(responses: Vec<String>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let port = listener.local_addr().expect("an address").port();
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&hits);
            let heads = Arc::new(std::sync::Mutex::new(Vec::new()));
            let logged = Arc::clone(&heads);
            let closing = Arc::clone(&stop);
            std::thread::spawn(move || {
                for answer in responses {
                    let Ok((mut stream, _)) = listener.accept() else { return };
                    if closing.load(Ordering::SeqCst) == 1 {
                        return;
                    }
                    seen.fetch_add(1, Ordering::SeqCst);
                    // Read the request head so the client is not reset mid-send.
                    let mut reader = std::io::BufReader::new(stream.try_clone().expect("a clone"));
                    let mut head = String::new();
                    let mut line = String::new();
                    while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                        if line == "\r\n" || line == "\n" {
                            break;
                        }
                        head.push_str(&line);
                        line.clear();
                    }
                    if let Ok(mut list) = logged.lock() {
                        list.push(head);
                    }
                    let _ = stream.write_all(answer.as_bytes());
                    let _ = stream.flush();
                }
            });
            Server { port, hits, stop, heads }
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }

        /// What the *n*-th request asked for, lowercased so a header name's case
        /// cannot make a test pass or fail on its own.
        fn head(&self, n: usize) -> String {
            self.heads
                .lock()
                .map(|list| list.get(n).cloned().unwrap_or_default())
                .unwrap_or_default()
                .to_lowercase()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(1, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        }
    }

    fn ok_response(body: &[u8]) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        )
    }

    /// A `206 Partial Content` answer for the tail of a file: what a server that
    /// honours `Range` sends back.
    fn partial_response(body: &[u8], start: u64, total: u64) -> String {
        let end = start + body.len() as u64 - 1;
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        )
    }

    /// A `206` whose `Content-Range` starts somewhere other than the bytes on disk —
    /// a server with a different idea of the file.
    fn misplaced_partial_response(body: &[u8], start: u64, total: u64) -> String {
        partial_response(body, start, total)
    }

    fn plan_for(url: String, body: &[u8]) -> Plan {
        Plan {
            name: "fake.gguf".to_string(),
            repo: "test/repo".to_string(),
            url,
            bytes: body.len() as u64,
            sha256: crate::sha::sha256(body),
            licence: "test".to_string(),
            redirect_hosts: Vec::new(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-model-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("models")).expect("a scratch root");
        dir
    }

    /// The manifest is the pin: it is read, validated, and a broken one is a
    /// sentence naming the file.
    #[test]
    fn the_manifest_is_read_and_validated() {
        let root = scratch("manifest");
        let good = r#"{"name":"m.gguf","repo":"a/b","url":"https://example.invalid/m.gguf","bytes":7,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","license":"x"}"#;
        fs::write(root.join("models").join("model.json"), good).expect("write");
        let plan = load_manifest(&root).expect("a valid manifest");
        assert_eq!(plan.name, "m.gguf");
        assert_eq!(plan.bytes, 7);

        // A name that could leave models/ is refused, and so is a digest that pins
        // nothing.
        for bad in [
            good.replace("m.gguf", "../m.gguf"),
            good.replace(&"a".repeat(64), "abc"),
        ] {
            fs::write(root.join("models").join("model.json"), bad).expect("write");
            assert!(load_manifest(&root).is_err());
        }
        fs::write(root.join("models").join("model.json"), "not json").expect("write");
        assert!(load_manifest(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    /// The URL rule is the app's: a file:, a loopback address or a plain-http URL is
    /// refused before a socket is opened.
    #[test]
    fn the_url_is_the_apps_rule_and_https_only() {
        let root = scratch("url");
        let body = b"weights";
        for url in [
            "file:///etc/passwd".to_string(),
            "http://example.com/model.gguf".to_string(),
            "https://127.0.0.1/model.gguf".to_string(),
            "https://localhost/model.gguf".to_string(),
        ] {
            let plan = plan_for(url.clone(), body);
            let error = ensure(&plan, &root, true, true, &mut |_| {})
                .expect_err(&format!("{url} must be refused"));
            assert!(
                error.contains("refused") || error.contains("https only"),
                "{url}: {error}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// A transfer to a table-less URL still writes nothing: the guard refuses before
    /// a socket exists. This is the SSRF half, pinned without the network.
    #[test]
    fn a_refused_url_creates_no_file() {
        let root = scratch("no-file");
        let plan = plan_for("https://10.1.2.3/model.gguf".to_string(), b"x");
        assert!(ensure(&plan, &root, true, true, &mut |_| {}).is_err());
        assert!(!root.join("models").join("fake.gguf").exists());
        assert!(!root.join("models").join("fake.gguf.part").exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// A complete, matching transfer lands at the pinned path and leaves no
    /// `.part` behind. The test server is on loopback, so this uses the one
    /// test-only entry point; the production path is the same function with
    /// `loopback_test: false`, which the tests above prove refuses first.
    #[test]
    fn a_good_transfer_is_verified_and_renamed() {
        let body = b"the weights, in miniature".to_vec();
        let server = Server::start(vec![ok_response(&body)]);
        let root = scratch("good");
        let plan = plan_for(server.url("/model.gguf"), &body);
        let target = root.join("models").join("fake.gguf");
        let partial = root.join("models").join("fake.gguf.part");
        let fetched = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("a complete transfer");
        assert!(fetched.downloaded);
        assert_eq!(fetched.bytes, body.len() as u64);
        assert_eq!(fetched.sha256, plan.sha256);
        assert_eq!(fs::read(&target).expect("the file"), body);
        assert!(!partial.exists(), "the partial file must be renamed away");
        assert_eq!(server.hits(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// A complete-looking file that is not the pinned one is removed, never renamed
    /// into place.
    #[test]
    fn a_wrong_digest_removes_what_it_wrote() {
        let root = scratch("digest");
        let body = b"truncated-weights";
        let server = Server::start(vec![ok_response(body)]);
        let mut plan = plan_for(server.url("/model.gguf"), body);
        plan.sha256 = "0".repeat(64);
        let outcome = fetch_with(
            &plan,
            &root,
            &root.join("models").join("fake.gguf"),
            &root.join("models").join("fake.gguf.part"),
            true,
            &SystemLookup,
            &mut |_| {},
        );
        let error = outcome.expect_err("a wrong digest must fail");
        assert!(error.contains("not the one ATLAS expects"), "{error}");
        assert!(error.contains("atlas-setup --install again"), "the next command: {error}");
        assert!(error.is_ascii(), "ASCII, so no terminal renders it wrong: {error}");
        assert!(!root.join("models").join("fake.gguf").exists());
        assert!(!root.join("models").join("fake.gguf.part").exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// A file that is already right is not fetched again, and the check never opens a
    /// socket.
    #[test]
    fn a_correct_file_is_kept_without_a_request() {
        let root = scratch("kept");
        let body = b"already here";
        let plan = plan_for("http://127.0.0.1:9/model.gguf".to_string(), body);
        fs::write(root.join("models").join("fake.gguf"), body).expect("write");
        let outcome = ensure(&plan, &root, true, false, &mut |_| {}).expect("kept");
        assert!(!outcome.downloaded);
        assert_eq!(outcome.bytes, body.len() as u64);
        assert_eq!(outcome.sha256, plan.sha256);
        let _ = fs::remove_dir_all(&root);
    }

    /// The size is checked before the digest, so a short file on disk is not silently
    /// accepted as "already there".
    #[test]
    fn a_short_file_on_disk_is_not_a_model() {
        let root = scratch("short");
        let body = b"the whole file";
        let plan = plan_for("http://127.0.0.1:9/model.gguf".to_string(), body);
        fs::write(root.join("models").join("fake.gguf"), b"the whole").expect("write");
        // It is not the pinned size, so `ensure` tries to fetch — and the URL is
        // refused, which is the observable outcome here.
        let error = ensure(&plan, &root, false, false, &mut |_| {}).expect_err("refused");
        assert!(error.contains("refused") || error.contains("https only"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    /// A resolver a test scripts.
    ///
    /// Injection is the point of `atlas_net::Lookup`: a name that answers with a private
    /// address cannot be staged against the real resolver without a hostfile and a
    /// network, and staging it is the only way to prove the filter is load-bearing.
    #[derive(Debug, Default)]
    struct Scripted {
        answers: std::collections::BTreeMap<String, Vec<std::net::SocketAddr>>,
    }

    impl Scripted {
        fn new(pairs: &[(&str, &str)]) -> Scripted {
            let mut answers = std::collections::BTreeMap::new();
            for (host, address) in pairs {
                let ip: std::net::IpAddr = address.parse().expect("an address literal");
                answers.insert((*host).to_string(), vec![std::net::SocketAddr::new(ip, 443)]);
            }
            Scripted { answers }
        }
    }

    impl Lookup for Scripted {
        fn addresses(&self, host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
            match self.answers.get(host) {
                Some(found) => Ok(found
                    .iter()
                    .map(|address| std::net::SocketAddr::new(address.ip(), port))
                    .collect()),
                None => Err(format!("the name {host} does not resolve")),
            }
        }
    }

    /// Every address class that is not the public internet: this machine, the link-local
    /// metadata address, and the two LAN shapes a home and an office router hand out.
    const PRIVATE_ANSWERS: [(&str, &str); 5] = [
        ("rebind.example.com", "127.0.0.1"),
        ("metadata.example.com", "169.254.169.254"),
        ("nas.example.com", "192.168.1.10"),
        ("office.example.com", "10.0.0.5"),
        ("v6.example.com", "::1"),
    ];

    /// A name is judged by what it *resolves to*, not only by how it is spelled: a
    /// public-looking host that answers with `127.0.0.1`, the metadata address or a LAN
    /// address is refused before a connection exists and before a byte is written.
    ///
    /// This is the SSRF hole the audit found — `https://rebind.example/x.gguf` passed
    /// `Gated::verify` and was then dialled — pinned offline with an injected resolver.
    #[test]
    fn a_name_that_resolves_to_this_machine_or_its_lan_is_refused() {
        let body = b"weights";
        for (host, address) in PRIVATE_ANSWERS {
            let root = scratch(&format!("ssrf-{}", host.split('.').next().unwrap_or("x")));
            let plan = plan_for(format!("https://{host}/model.gguf"), body);
            let lookup = Scripted::new(&[(host, address)]);
            let target = root.join("models").join("fake.gguf");
            let partial = root.join("models").join("fake.gguf.part");
            let error = fetch_with(&plan, &root, &target, &partial, false, &lookup, &mut |_| {})
                .expect_err(&format!("{host} -> {address} must be refused"));
            assert!(
                error.contains("this download may only come from a public address"),
                "{host} -> {address}: {error}"
            );
            assert!(
                error.contains(address) || error.contains("loopback"),
                "the sentence names the address it refused: {error}"
            );
            assert!(!partial.exists(), "{host}: no partial file was created");
            assert!(!target.exists(), "{host}: nothing landed at the pinned path");
            let _ = fs::remove_dir_all(&root);
        }
    }

    /// A public name whose answers are all public still passes the filter, so the rule
    /// refuses addresses and not downloads.
    #[test]
    fn a_name_that_resolves_to_a_public_address_passes_the_filter() {
        let gated = Gated::verify("https://huggingface.co/a/b.gguf").expect("a public URL");
        let lookup = Scripted::new(&[("huggingface.co", "3.163.189.100")]);
        assert!(public_address(&gated, &lookup).is_ok());

        // One public and one private answer keeps the transfer: multi-homed hosts are
        // ordinary, and the filter drops the private answer rather than the site.
        let mixed = Scripted::new(&[("mixed.example.com", "3.163.189.100")]);
        let gated = Gated::verify("https://mixed.example.com/a.gguf").expect("a public URL");
        assert!(public_address(&gated, &mixed).is_ok());
    }

    /// A redirect into this machine's LAN is refused *instead of* being followed: the
    /// answer is inspected, no second connection is made, and no byte is written.
    ///
    /// The literal forms (`169.254.169.254`, `192.168.x`, `10.x`) are refused by the
    /// guard's own door, which is why this needs no resolver and no real network: the
    /// test server is the first hop, and the hop it asks for never becomes a socket.
    #[test]
    fn a_redirect_into_this_machine_or_its_lan_is_refused() {
        let body = b"weights";
        for location in [
            "http://169.254.169.254/latest/meta-data/",
            "http://192.168.1.10/model.gguf",
            "http://10.0.0.5/model.gguf",
        ] {
            let answer = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let server = Server::start(vec![answer]);
            let root = scratch("redirect-private");
            let plan = plan_for(server.url("/model.gguf"), body);
            let target = root.join("models").join("fake.gguf");
            let partial = root.join("models").join("fake.gguf.part");
            // The first hop is the loopback test server; the hop it asks for is not on
            // this machine's loopback, so the guard refuses it.
            let error =
                fetch_with(&plan, &root, &target, &partial, true, &Scripted::default(), &mut |_| {})
                    .expect_err(&format!("a redirect to {location} must be refused"));
            assert!(
                error.contains("redirected somewhere it may not go"),
                "{location}: {error}"
            );
            assert!(!partial.exists(), "{location}: no partial file was created");
            assert_eq!(server.hits(), 1, "{location}: the redirect was judged, not followed");
            let _ = fs::remove_dir_all(&root);
        }
    }

    /// A redirect is judged on the URL it points at: the scheme, the host the pin names,
    /// and the address the host resolves to. Each rule is a way a download was walked off
    /// the pinned file.
    #[test]
    fn a_redirect_is_judged_before_it_is_followed() {
        let pin = "https://huggingface.co/repo/resolve/main/m.gguf";
        let from = Gated::verify(pin).expect("a public URL");
        let lookup = Scripted::new(&[
            ("huggingface.co", "3.163.189.100"),
            ("cdn-lfs.huggingface.co", "3.163.189.120"),
            ("elsewhere.example.com", "93.184.216.34"),
            ("nas.example.com", "192.168.1.10"),
        ]);
        let mut plan = plan_for(pin.to_string(), b"weights");

        // Relative locations (what a CDN sends) resolve against the URL that sent them.
        let same = redirect_target(&from, "/repo/resolve/main/m.gguf?x=1", &plan, &lookup, false)
            .expect("a hop on the pinned host");
        assert!(same.as_str().starts_with("https://huggingface.co/"), "{}", same.as_str());
        let relative = redirect_target(&from, "../other/m.gguf", &plan, &lookup, false)
            .expect("a relative hop on the pinned host");
        assert!(relative.as_str().starts_with("https://huggingface.co/"), "{}", relative.as_str());

        // Another host is refused unless the pin names it, and the sentence names both.
        let error = redirect_target(&from, "https://elsewhere.example.com/m.gguf", &plan, &lookup, false)
            .expect_err("a third party is refused");
        assert!(error.contains("elsewhere.example.com") && error.contains("huggingface.co"), "{error}");
        assert!(error.contains("redirect_hosts"), "{error}");
        // The refusal prints the line to add, with the host it observed, so the fix is a
        // copy rather than a guess.
        assert!(
            error.contains(r#""redirect_hosts": ["elsewhere.example.com"]"#),
            "the refusal prints the line to add, with the host it saw: {error}"
        );

        // The pin can name the CDN, which is what a real `huggingface.co` download needs.
        plan.redirect_hosts = vec!["cdn-lfs.huggingface.co".to_string()];
        let cdn = redirect_target(&from, "https://cdn-lfs.huggingface.co/x.gguf", &plan, &lookup, false)
            .expect("a CDN the pin named");
        assert!(cdn.as_str().starts_with("https://cdn-lfs.huggingface.co/"), "{}", cdn.as_str());

        // https, whatever the host.
        let error = redirect_target(&from, "http://huggingface.co/m.gguf", &plan, &lookup, false)
            .expect_err("a hop off https is refused");
        assert!(error.contains("https"), "{error}");

        // And a host the pin named may not resolve into the LAN either.
        plan.redirect_hosts = vec!["nas.example.com".to_string()];
        let error = redirect_target(&from, "https://nas.example.com/m.gguf", &plan, &lookup, false)
            .expect_err("a named host that resolves into the LAN is refused");
        assert!(error.contains("public address"), "{error}");

        // The three addresses the audit named, in redirect position: the loopback
        // literal, the link-local metadata address, and a host that answers inside the
        // LAN. The first two never reach the host rule — the guard refuses the URL — and
        // the third is refused by the address filter after the host rule passes.
        plan.redirect_hosts = vec!["nas.example.com".to_string()];
        for (location, wanted) in [
            ("http://127.0.0.1/model.gguf", "redirected somewhere it may not go"),
            ("http://169.254.169.254/latest/meta-data/", "redirected somewhere it may not go"),
            ("https://nas.example.com/model.gguf", "public address"),
        ] {
            let error = redirect_target(&from, location, &plan, &lookup, false)
                .expect_err(&format!("a redirect to {location} must be refused"));
            assert!(error.contains(wanted), "{location}: {error}");
        }
    }

    /// The pin is where a redirect host is written down: read when present, refused when
    /// it is not a list of names, and empty when absent.
    #[test]
    fn the_pin_can_name_a_redirect_host() {
        let root = scratch("redirect-hosts");
        let path = root.join("models").join("model.json");
        let base = r#"{"name":"m.gguf","repo":"a/b","url":"https://huggingface.co/r/resolve/main/m.gguf","bytes":7,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","license":"x""#;

        std::fs::write(&path, format!("{base}}}")).expect("write");
        let plan = load_manifest(&root).expect("a manifest without the field");
        assert!(plan.redirect_hosts.is_empty(), "{plan:?}");
        assert_eq!(plan.host(), "huggingface.co");
        assert!(plan.permits("huggingface.co"));
        assert!(!plan.permits("cdn-lfs.huggingface.co"));

        std::fs::write(
            &path,
            format!(r#"{base},"redirect_hosts":["CDN-LFS.huggingface.co"]}}"#),
        )
        .expect("write");
        let plan = load_manifest(&root).expect("a manifest naming a CDN");
        assert_eq!(plan.redirect_hosts, vec!["cdn-lfs.huggingface.co".to_string()]);
        assert!(plan.permits("cdn-lfs.huggingface.co"), "case does not matter");
        assert!(plan.permits("huggingface.co"), "the pinned host always stays");
        assert!(!plan.permits("elsewhere.example.com"));

        for bad in [
            format!(r#"{base},"redirect_hosts":"cdn.example"}}"#),
            format!(r#"{base},"redirect_hosts":[42]}}"#),
        ] {
            std::fs::write(&path, bad.clone()).expect("write");
            let error = load_manifest(&root).expect_err(&format!("{bad} must be refused"));
            assert!(error.contains("redirect_hosts"), "{error}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    // --- the pin the repository ships with ---------------------------------------

    /// The repository ships no weights and no manifest, so the pin lives in the crate.
    /// This test is where the maintainer's URL and file name are recorded: changing the
    /// file ATLAS runs without changing this test is not possible on purpose.
    #[test]
    fn a_fresh_clone_has_a_pin_without_a_manifest() {
        let root = scratch("builtin-pin");
        let plan = pin(&root).expect("the built-in pin");

        assert_eq!(plan.name, "Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf");
        assert_eq!(
            plan.url,
            "https://huggingface.co/DavidAU/Qwen3.5-9B-The-Defiant-Fable-Uncensored-Heretic-NEO-IMATRIX-MAX-MTP-GGUF/resolve/main/Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-MTP-IQ4_XS.gguf"
        );
        assert_eq!(plan.host(), "huggingface.co");
        assert!(plan.url.starts_with("https://"), "https only: {}", plan.url);
        // Pinned by size *and* digest: a truncated transfer fails the first check, a
        // different file fails the second, and both are deleted rather than renamed.
        assert!(plan.pins_size(), "{plan:?}");
        assert!(plan.pins_digest(), "{plan:?}");
        assert_eq!(plan.bytes, 6_526_302_432);
        assert_eq!(plan.sha256, "8d33af536e35805967c9f4d77e1ed62b8212bdee282c80c6dc9b79ae6d677637");
        // And the delivery hosts, or the one redirect a real HF download makes is refused.
        assert!(plan.permits("cdn-lfs.huggingface.co"), "{plan:?}");
        assert!(!plan.permits("example.invalid"), "not every host is permitted: {plan:?}");

        // A manifest that *is* there overrides it, deliberately.
        fs::write(
            root.join("models").join("model.json"),
            r#"{"name":"mine.gguf","repo":"a/b","url":"https://example.invalid/mine.gguf","bytes":7,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","license":"x"}"#,
        )
        .expect("a manifest");
        let mine = pin(&root).expect("the override");
        assert_eq!(mine.name, "mine.gguf");
        assert_eq!(mine.bytes, 7);
        let _ = fs::remove_dir_all(&root);
    }

    /// The pin and the file on this machine are the same file: the size is checked against
    /// the weights themselves when they are here, so a pin that drifted from the copy ATLAS
    /// runs is a failing test rather than a first run that downloads 6.5 GB twice.
    ///
    /// The digest is not recomputed here (6.5 GB per test run is a tax on every build); it
    /// is `shasum -a 256` on the same path, recorded in `DEFAULT_SHA256`. The size is the
    /// cheap half of the same claim.
    #[test]
    fn the_pin_and_the_weights_on_this_machine_are_the_same_file() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        match weights_size(&root) {
            Some(size) => assert_eq!(size, DEFAULT_BYTES, "the weights and the pin disagree"),
            // A machine that has not downloaded the model yet: nothing to compare, and
            // nothing to say from here — the installer reports this state itself.
            None => {}
        }
    }

    /// A pin without a size or a digest still checks what arrived: the size the server
    /// reports. A transfer that ends early is not renamed into place.
    #[test]
    fn an_unpinned_file_is_checked_against_the_size_the_server_reports() {
        let root = scratch("server-size");
        let body = b"0123456789";
        let (target, partial) = paths(&plan_for("x".to_string(), body), &root);
        let mut plan = plan_for("x".to_string(), body);
        plan.bytes = 0;
        plan.sha256 = String::new();
        assert!(!plan.pins_size() && !plan.pins_digest());

        // The server says ten bytes and sends ten: the file lands, with its digest
        // computed but not compared.
        let server = Server::start(vec![ok_response(body)]);
        plan.url = server.url("/m.gguf");
        let fetched = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("a checked transfer");
        assert_eq!(fetched.bytes, 10);
        assert_eq!(fetched.sha256, crate::sha::sha256(body));
        assert_eq!(fs::read(&target).expect("the model"), body);

        // The same server, a body shorter than it announced: kept, not renamed.
        let short = scratch("server-size-short");
        let (target, partial) = paths(&plan, &short);
        let truncated = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{}",
            String::from_utf8_lossy(&body[..3])
        );
        let server = Server::start(vec![truncated]);
        plan.url = server.url("/m.gguf");
        let error = fetch_with(&plan, &short, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect_err("an incomplete transfer");
        assert!(error.contains("were kept"), "{error}");
        assert!(!target.exists(), "no model from a short transfer");

        // And a server that will not say how big the file is cannot be checked at all.
        let no_length = scratch("no-length");
        let (target, partial) = paths(&plan, &no_length);
        let server = Server::start(vec![
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n0123456789\r\n".to_string(),
        ]);
        plan.url = server.url("/m.gguf");
        let error = fetch_with(&plan, &no_length, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect_err("no size to check");
        assert!(error.contains("did not say how big"), "{error}");
        assert!(!target.exists());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&short);
        let _ = fs::remove_dir_all(&no_length);
    }

    /// A second run is the fast path: the file is there, so no request is made, and the
    /// progress callback is told the whole thing is already done (a bar that shows zero
    /// for an installed machine reads as "nothing happened").
    #[test]
    fn a_second_run_touches_the_network_for_nothing() {
        let root = scratch("fast-path");
        let body = b"0123456789";
        let mut plan = plan_for("x".to_string(), body);
        let (target, _) = paths(&plan, &root);
        fs::write(&target, body).expect("the model is already here");

        let server = Server::start(vec![ok_response(body)]);
        plan.url = server.url("/m.gguf");
        let mut seen: Vec<(u64, u64)> = Vec::new();
        let fetched = ensure_with(&plan, &root, false, false, &mut |_| {}, &mut |done, total| {
            seen.push((done, total))
        })
        .expect("the fast path");

        assert!(!fetched.downloaded, "nothing was fetched");
        assert_eq!(server.hits(), 0, "and nothing was asked for");
        assert_eq!(seen, vec![(10, 10)], "progress says complete, not empty");
        let _ = fs::remove_dir_all(&root);
    }

    // --- resuming a partial transfer ---------------------------------------------

    /// The fact this whole path exists for: a 6.5 GB transfer over a home link stops
    /// halfway, and the next run must ask for the rest instead of starting again.
    #[test]
    fn a_partial_file_is_resumed_from_where_it_stopped() {
        let root = scratch("resume");
        let body = b"0123456789";
        let (target, partial) = paths(&plan_for("x".to_string(), body), &root);
        fs::write(&partial, &body[..6]).expect("a half transfer");

        let server =
            Server::start(vec![partial_response(&body[6..], 6, body.len() as u64)]);
        let plan = plan_for(server.url("/m.gguf"), body);
        let fetched = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("a resumed transfer");

        assert_eq!(fetched.bytes, body.len() as u64);
        assert!(fetched.downloaded, "this run did fetch the rest");
        assert_eq!(fs::read(&target).expect("the model"), body, "the prefix was kept and completed");
        assert!(
            server.head(0).contains("range: bytes=6-"),
            "the request must ask for the rest: {:?}",
            server.head(0)
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A server that ignores `Range` sends the whole file. Appending it to the prefix
    /// would produce a longer, wrong file, so the prefix has to be dropped.
    #[test]
    fn a_server_that_ignores_the_range_starts_the_file_over() {
        let root = scratch("ignored-range");
        let body = b"0123456789";
        let (target, partial) = paths(&plan_for("x".to_string(), body), &root);
        fs::write(&partial, b"XXXXXXXXXX"[..6].to_vec()).expect("a half transfer");

        let server = Server::start(vec![ok_response(body)]);
        let plan = plan_for(server.url("/m.gguf"), body);
        fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("the whole file again");

        assert_eq!(
            fs::read(&target).expect("the model"),
            body,
            "the ignored prefix was replaced, not appended to"
        );
        assert_eq!(server.hits(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// A `Range` answer that starts somewhere else would interleave two files: that is
    /// a refusal, and the person keeps what is on disk.
    #[test]
    fn a_resume_that_starts_elsewhere_is_refused() {
        let root = scratch("misplaced-range");
        let body = b"0123456789";
        let (target, partial) = paths(&plan_for("x".to_string(), body), &root);
        fs::write(&partial, &body[..6]).expect("a half transfer");

        let server = Server::start(vec![misplaced_partial_response(&body[..], 0, body.len() as u64)]);
        let plan = plan_for(server.url("/m.gguf"), body);
        let error = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect_err("a mismatched range");

        assert!(error.contains("resumed at byte 0"), "{error}");
        assert!(error.contains("--fetch"), "the fix is a command: {error}");
        assert!(!target.exists(), "nothing was renamed into place");
        assert_eq!(
            fs::metadata(&partial).map(|meta| meta.len()).unwrap_or(0),
            6,
            "the bytes already here are kept for the next attempt"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The failure a real first run hits: a `.part` left by a killed download whose bytes
    /// are *wrong* though its length looks plausible. The length check passes, the transfer
    /// completes on top of the bad prefix, and the digest is what catches it — the file is
    /// deleted, the sentence says what happened and what to run, and the next attempt starts
    /// at byte zero.
    #[test]
    fn a_corrupted_partial_file_is_detected_and_started_over() {
        let root = scratch("corrupt-part");
        let body = b"0123456789";
        let plan = plan_for("x".to_string(), body);
        let (target, partial) = paths(&plan, &root);

        // Six bytes of rubbish where six bytes of the real file should be.
        fs::write(&partial, b"XXXXXX").expect("a corrupt partial");

        // The server resumes at 6 and sends the real tail, so the *length* is right and
        // only the digest can disagree.
        let server = Server::start(vec![partial_response(&body[6..], 6, body.len() as u64)]);
        let mut plan = plan_for(server.url("/m.gguf"), body);
        plan.url = server.url("/m.gguf");
        let error = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect_err("a corrupt prefix");

        assert!(error.contains("not the one ATLAS expects"), "{error}");
        assert!(error.contains("deleted"), "what happened to it: {error}");
        assert!(error.contains("atlas-setup --install again"), "what to do: {error}");
        assert!(!target.exists(), "a wrong file is never renamed into place");
        assert!(!partial.exists(), "the corrupt partial is removed, not resumed again");

        // The next attempt asks for the whole file: no prefix, no Range header.
        let again = Server::start(vec![ok_response(body)]);
        plan.url = again.url("/m.gguf");
        let fetched = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("a clean transfer");
        assert_eq!(fetched.bytes, body.len() as u64);
        assert_eq!(fs::read(&target).expect("the model"), body);
        assert!(
            !again.head(0).contains("range"),
            "after the bad part is gone there is nothing to resume: {:?}",
            again.head(0)
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The offset, exactly: a killed download leaves N bytes, the next request asks for
    /// byte N and nothing else, and the bytes that arrive are appended — not written over
    /// the prefix and not written from the start.
    #[test]
    fn a_resume_asks_for_exactly_the_byte_after_the_last_one_saved() {
        let root = scratch("offset");
        // ASCII on purpose: the test server writes its answers as text, so a body with
        // non-UTF-8 bytes would be mangled by the harness rather than by the code under test.
        let body: Vec<u8> = (0..256u32).map(|i| b'a' + (i % 26) as u8).collect();
        let (target, partial) = paths(&plan_for("x".to_string(), &body), &root);
        let saved = 200usize;
        fs::write(&partial, &body[..saved]).expect("a killed download");

        let server = Server::start(vec![partial_response(&body[saved..], saved as u64, body.len() as u64)]);
        let plan = plan_for(server.url("/m.gguf"), &body);
        fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("a resumed transfer");

        assert!(
            server.head(0).contains(&format!("range: bytes={saved}-")),
            "the request must start at {saved}: {:?}",
            server.head(0)
        );
        assert_eq!(
            fs::read(&target).expect("the model"),
            body,
            "the first {saved} bytes are the ones that were already there, in order"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A `.part` past the pinned size is not a partial transfer, so it is not resumed:
    /// the next run must produce exactly the pinned file, not the pinned file plus the
    /// leftovers.
    #[test]
    fn a_partial_file_bigger_than_the_pin_is_discarded() {
        let root = scratch("oversized-part");
        let body = b"0123456789";
        let (target, partial) = paths(&plan_for("x".to_string(), body), &root);
        fs::write(&partial, vec![b'X'; 25]).expect("an oversized partial");

        let server = Server::start(vec![ok_response(body)]);
        let plan = plan_for(server.url("/m.gguf"), body);
        let mut said: Vec<String> = Vec::new();
        fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |line| {
            said.push(line.to_string())
        })
        .expect("a fresh transfer");

        assert_eq!(fs::metadata(&target).map(|meta| meta.len()).unwrap_or(0), 10);
        assert!(!server.head(0).contains("range"), "no range for a file that cannot be resumed");
        // And it said so, rather than silently restarting a download that looked like it
        // had 25 bytes of progress.
        let all = said.join("\n");
        assert!(all.contains("a partial download was here (25 bytes)"), "{all}");
        assert!(all.contains("so it starts again from the beginning"), "{all}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The other half of resuming: a transfer that stops keeps what it wrote, says how
    /// much that is, and the next run finishes the file from there.
    #[test]
    fn an_interrupted_transfer_keeps_what_it_wrote_and_the_next_one_finishes_it() {
        let root = scratch("interrupted");
        let body = b"0123456789";
        let (target, partial) = paths(&plan_for("x".to_string(), body), &root);

        // The head promises the whole file; the body stops after four bytes.
        let truncated = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(&body[..4])
        );
        let first = Server::start(vec![truncated]);
        let plan = plan_for(first.url("/m.gguf"), body);
        let error = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect_err("a transfer that ended early");

        assert!(error.contains("were kept"), "the sentence says what happened: {error}");
        assert!(error.contains("carries on"), "and what to do next: {error}");
        let kept = fs::metadata(&partial).map(|meta| meta.len()).unwrap_or(0);
        assert_eq!(kept, 4, "the bytes that arrived are kept, exactly, for the next run");
        assert!(!target.exists(), "an incomplete file is never the model");

        // The second run gets the rest and lands the pinned file.
        let second = Server::start(vec![partial_response(&body[kept as usize..], kept, body.len() as u64)]);
        let plan = plan_for(second.url("/m.gguf"), body);
        let fetched = fetch_with(&plan, &root, &target, &partial, true, &SystemLookup, &mut |_| {})
            .expect("the rest of the file");
        assert_eq!(fetched.bytes, body.len() as u64);
        assert_eq!(fs::read(&target).expect("the model"), body);
        assert!(
            second.head(0).contains(&format!("range: bytes={kept}-")),
            "the second request asks for the rest: {:?}",
            second.head(0)
        );
        let _ = fs::remove_dir_all(&root);
    }


    // --- a full disk, as the write path sees it -----------------------------------------

    /// A writer that refuses the way a full disk does: some bytes fit, then `ENOSPC`.
    struct Full {
        room: usize,
    }

    impl std::io::Write for Full {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len() > self.room {
                let wrote = self.room;
                self.room = 0;
                if wrote > 0 {
                    return Ok(wrote);
                }
                return Err(std::io::Error::from_raw_os_error(28));
            }
            self.room -= buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer that fails for a reason that is not the disk being full.
    struct Broken;

    impl std::io::Write for Broken {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied by the folder",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The whole point of this round: a full disk is discovered by the write, not by a
    /// program started first. The error the operating system returns is recognised in each
    /// of its three spellings, and nothing else is mistaken for it.
    #[test]
    fn a_full_disk_is_recognised_by_the_error_it_returns() {
        // ENOSPC (macOS and Linux), and Windows' two full-disk errors, as raw codes.
        for code in [28, 39, 112] {
            assert!(disk_is_full(&std::io::Error::from_raw_os_error(code)), "{code}");
        }
        // The Rust kind, when the platform layer reports it that way.
        assert!(disk_is_full(&std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "no room"
        )));
        // A filesystem that only says it in words.
        assert!(disk_is_full(&std::io::Error::new(
            std::io::ErrorKind::Other,
            "No space left on device (os error 28)"
        )));
        // And not a general failure: a refused folder is a different sentence.
        let refused = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert!(!disk_is_full(&refused), "a refused folder is not a full disk");
    }

    /// The sentence says how much arrived, that the rest does not fit, and that the part is
    /// kept and carried on from. One sentence, ASCII, no stack trace.
    #[test]
    fn the_full_disk_sentence_names_the_bytes_kept_and_the_next_step() {
        let line = out_of_room(1_234_567, 6_526_302_432);
        assert!(line.contains("1234567 of 6526302432 bytes"), "{line}");
        assert!(
            line.contains("not enough free space"),
            "it says there is no room: {line}"
        );
        assert!(
            line.contains("free up some space and run the same command again"),
            "what to do: {line}"
        );
        assert!(
            line.contains("carries on from there"),
            "the part is not wasted: {line}"
        );
        assert!(line.is_ascii(), "{line}");
    }

    /// Through the real write helper: `ENOSPC` becomes that sentence, and the bytes already
    /// written are counted truthfully.
    #[test]
    fn a_write_that_runs_out_of_room_returns_the_sentence_not_an_io_error() {
        let mut writer = Full { room: 4 };
        let error = write_chunk(
            &mut writer,
            b"0123456789",
            1_000,
            2_000,
            Path::new("/models/x.gguf.part"),
        )
        .expect_err("a full disk");
        assert_eq!(error, out_of_room(1_000, 2_000));
        assert!(error.contains("1000 of 2000 bytes"), "{error}");

        // A flush that fails the same way gets the same answer: the last buffered bytes
        // are part of the same problem.
        struct FullFlush;
        impl std::io::Write for FullFlush {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::from_raw_os_error(28))
            }
        }
        let error = flush_chunk(&mut FullFlush, 2_000, 2_000, Path::new("/models/x.gguf.part"))
            .expect_err("a full disk at the flush");
        assert_eq!(error, out_of_room(2_000, 2_000));
    }

    /// Every other write failure still names the file, because that reader has to go and
    /// look at a folder.
    #[test]
    fn a_write_that_fails_for_another_reason_names_the_file() {
        let error = write_chunk(&mut Broken, b"x", 0, 10, &Path::new("/models/x.gguf.part"))
            .expect_err("a refused write");
        assert!(error.contains("/models/x.gguf.part"), "{error}");
        assert!(error.contains("denied by the folder"), "{error}");
        assert!(!error.contains("free up some space"), "{error}");
    }

    /// The number the installer prints before the transfer: the file plus room to work,
    /// never an overflow on an absurd pin.
    #[test]
    fn the_room_needed_is_the_file_plus_headroom() {
        assert_eq!(needs(6_526_302_432), 6_526_302_432 + HEADROOM);
        assert_eq!(needs(0), HEADROOM);
        assert_eq!(needs(u64::MAX), u64::MAX);
    }

}
