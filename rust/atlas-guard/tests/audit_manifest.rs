//! The build is part of the attack surface — checked on the manifests, the lock
//! and the scripts that drive cargo.
//!
//! A dependency decides more than "code we did not write": its *features* decide
//! whether a decompressor is linked into the HTTP client, whether the wasm page
//! can call `eval`, and whether a build script probes the machine for a system
//! library. None of that is visible in a review of `src/`, so it is checked here,
//! on the three files that do say it: `rust/Cargo.toml`, `rust/Cargo.lock`, and
//! the scripts that invoke cargo.
//!
//! What each test is defending, in one line:
//!
//! * `every_dependency_is_pinned_to_an_exact_version` — a caret range is a silent
//!   SBOM change; `--locked` can only refuse drift if every pin is exact.
//! * `no_dependency_is_declared_with_its_own_default_features` — a default feature
//!   is somebody else's list of what this binary needs, and it is how `gzip`,
//!   `unsafe-eval` and a system-library probe arrive without a decision.
//! * `the_http_client_links_no_decompressor_and_no_charset_decoder` — the page cap
//!   bounds bytes *read*; it cannot bound what a decompressor allocates on the way
//!   there, and `atlas-net` decodes charsets itself.
//! * `nothing_that_fetches_a_body_links_a_decompressor` — the lock, not only the
//!   manifest, is what links the compression crates (a transitive edge is enough),
//!   so the ban is stated per binary: a crate that links an HTTP client links no
//!   decoder, and every decoder that *is* linked is recorded with what pulls it.
//! * `no_dependency_comes_from_anywhere_but_crates_io` — a `git` dep, a second
//!   registry or a `[patch]` table is a source swap with no checksum to notice it.
//! * `no_member_carries_a_build_script` — our own crates run no code at build time.
//! * `the_one_release_profile_is_the_workspace_one` — cargo *ignores* a profile in
//!   a member and only warns; a member that carries one claims hardening that was
//!   never applied.
//! * `every_cargo_invocation_is_locked` — the scripts, not only the docs, are what
//!   a human runs; a build without `--locked` may re-resolve.
//! * `the_lock_holds_a_checksum_for_everything_it_names` — an offline build of a
//!   cached tree still verifies what it compiled.
//! * `the_desktop_links_no_http_client` — the product's face is a native window
//!   now; a client behind it would make "fully local" a claim about our own
//!   `src/` only, with somebody else's socket in the binary.
//! * `the_desktop_opens_the_archive_read_only_and_writes_nowhere` — the archive
//!   belongs to the engines; one optimistic write from a view is the one writer
//!   nobody serialized.
//! * `the_desktop_opens_no_listener_and_starts_no_process` — a bind is the old
//!   server back, a spawn is archive text on its way to the shell, and the one
//!   socket the window does open is a loopback literal in one recorded file.
//! * `atlas_desktop_is_a_member_with_pinned_dependencies` — its four pins are
//!   declared inline, in a table the workspace rules never read.
//!
//! It reads text with `std` only: this crate has no dependencies, and a test that
//! needed a TOML parser to prove the workspace has no dependencies would be a
//! joke at its own expense.

use std::path::{Path, PathBuf};

// The desktop rules read shipped source as well as manifests, so they ask the one
// scanner in this crate what \"code\" means instead of trimming lines by hand —
// that is where a comment-out and a needle split across lines get their answer.
use atlas_guard::source::{find_wrapped, shipped_code, SourceFile};

/// `rust/` — the directory this test's crate sits in, one level up.
fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The workspace manifest, `rust/Cargo.toml`.
fn workspace_manifest() -> String {
    read(&rust_dir().join("Cargo.toml"))
}

/// `rust/Cargo.lock`.
fn lock_file() -> String {
    read(&rust_dir().join("Cargo.lock"))
}

/// One `name = value` entry of `[workspace.dependencies]` — see [`table_entries`].
fn workspace_dependencies(manifest: &str) -> Vec<(String, String)> {
    table_entries(manifest, "[workspace.dependencies]")
}

/// One `name = value` entry of the dependency table called `table`, with `value`
/// kept as written (`"1.0.0"`, or `{ version = "…", features = […] }` joined onto
/// one line).
///
/// Hand-parsed on purpose: an entry may span lines (the `web-sys` feature list
/// does), so a line is a *continuation* until the braces it opened are closed.
///
/// `table` is a parameter because there are two manifests that declare
/// dependencies and only one of them is the workspace's: `atlas-desktop` pins its
/// four inline (in `atlas-desktop/Cargo.toml`), so a rule that only read
/// `[workspace.dependencies]` would not see the crate that draws the window at
/// all. Same parser, same rules, one more table.
/// Every dependency `atlas-desktop/Cargo.toml` declares: the plain `[dependencies]` table
/// **and** each `[target.'cfg(…)'.dependencies]` one.
///
/// The window's dependencies are platform-conditioned now — `wry` and, as the manifest
/// stood when this was written, `serde_json` live under a macOS/Windows cfg — and a rule
/// that read only the plain table saw three of five. That is how the "pinned exactly" and
/// "no HTTP client" rules both went red at once: not because a dependency was wrong, but
/// because the parser stopped at the first table.
fn desktop_dependencies(manifest: &str) -> Vec<(String, String)> {
    let mut out = table_entries(manifest, "[dependencies]");
    for line in manifest.lines() {
        let header = line.trim();
        if header.starts_with("[target.") && header.ends_with(".dependencies]") {
            out.extend(table_entries(manifest, header));
        }
    }
    out
}

fn table_entries(manifest: &str, table: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut inside = false;
    let mut open: Option<(String, String)> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if let Some(entry) = open.take() {
                out.push(entry);
            }
            inside = trimmed == table;
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((_, value)) = open.as_mut() {
            if trimmed.is_empty() {
                continue;
            }
            value.push(' ');
            value.push_str(trimmed);
            let depth = value.matches('{').count() as i64 - value.matches('}').count() as i64;
            if depth <= 0 {
                out.push(open.take().unwrap());
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq) = trimmed.find('=') else { continue };
        let name = trimmed[..eq].trim().to_string();
        let value = trimmed[eq + 1..].trim().to_string();
        let depth = value.matches('{').count() as i64 - value.matches('}').count() as i64;
        if value.starts_with('{') && depth > 0 {
            open = Some((name, value));
        } else {
            out.push((name, value));
        }
    }
    if let Some(entry) = open.take() {
        out.push(entry);
    }
    out
}

/// The version a `[workspace.dependencies]` value declares: `"1.2.3"` or
/// `{ version = "1.2.3", … }`.
fn declared_version(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split('"').next();
    }
    let marker = "version";
    let at = value.find(marker)?;
    let rest = &value[at + marker.len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    rest.split('"').next()
}

/// The members the workspace builds, from `members = [ … ]`.
///
/// A member line is `"atlas-lib",` or a comment; the name is what sits between
/// the first pair of quotes.
fn members(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if !inside {
            if trimmed.starts_with("members") && trimmed.contains('[') {
                inside = !trimmed.contains(']');
            }
            continue;
        }
        if trimmed.starts_with(']') {
            break;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let mut pieces = trimmed.split('"');
        pieces.next();
        if let Some(name) = pieces.next() {
            let name = name.trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// One `[[package]]` block of `Cargo.lock`.
struct Locked {
    name: String,
    version: String,
    source: String,
    checksum: bool,
}

fn locked_packages(lock: &str) -> Vec<Locked> {
    let mut out: Vec<Locked> = Vec::new();
    let mut name = String::new();
    let mut version = String::new();
    let mut source = String::new();
    let mut checksum = false;
    let mut in_block = false;
    let flush = |name: &mut String,
                     version: &mut String,
                     source: &mut String,
                     checksum: &mut bool,
                     out: &mut Vec<Locked>| {
        if !name.is_empty() {
            out.push(Locked {
                name: std::mem::take(name),
                version: std::mem::take(version),
                source: std::mem::take(source),
                checksum: *checksum,
            });
        }
        *checksum = false;
    };
    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            if in_block {
                flush(&mut name, &mut version, &mut source, &mut checksum, &mut out);
            }
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name = ") {
            name = rest.trim_matches('"').to_string();
        } else if let Some(rest) = trimmed.strip_prefix("version = ") {
            version = rest.trim_matches('"').to_string();
        } else if let Some(rest) = trimmed.strip_prefix("source = ") {
            source = rest.trim_matches('"').to_string();
        } else if trimmed.starts_with("checksum = ") {
            checksum = true;
        }
    }
    if in_block {
        flush(&mut name, &mut version, &mut source, &mut checksum, &mut out);
    }
    out
}

/// Would this dependency value hand a build script to `rustc`?
fn has_build_script(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[build-dependencies]" || trimmed.starts_with("[build-dependencies.") {
            return Some(trimmed.to_string());
        }
        if trimmed.starts_with("build = ") || trimmed.starts_with("build=") {
            return Some(trimmed.to_string());
        }
    }
    None
}

#[test]
fn every_dependency_is_pinned_to_an_exact_version() {
    let manifest = workspace_manifest();
    let entries = workspace_dependencies(&manifest);
    // Six workspace pins since this task dropped the seven that only the web face used
    // (axum, tower, futures-core, tungstenite, wasm-bindgen*, js-sys, web-sys): serde,
    // serde_json, rusqlite, ureq, url, tokio. A floor, because a seventh pin is a
    // decision; the parser reading four would be a bug.
    assert!(
        entries.len() >= 6,
        "the parser read only {} dependencies out of [workspace.dependencies]",
        entries.len()
    );
    let mut wrong = Vec::new();
    for (name, value) in &entries {
        match declared_version(value) {
            // `=1.2.3` is exact. Anything else (`^`, `~`, a bare `1.2.3`) is a
            // range cargo may walk without anyone editing this file.
            Some(version) if version.starts_with('=') => {}
            Some(version) => wrong.push(format!("{name}: version = \"{version}\" is a range")),
            None => wrong.push(format!("{name}: no version at all ({value})")),
        }
    }
    assert!(
        wrong.is_empty(),
        "a dependency is not pinned exactly:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn no_dependency_is_declared_with_its_own_default_features() {
    let entries = workspace_dependencies(&workspace_manifest());
    let mut wrong = Vec::new();
    for (name, value) in &entries {
        if !value.contains("default-features = false") {
            wrong.push(format!("{name}: {}", value.trim()));
        }
    }
    assert!(
        wrong.is_empty(),
        "a dependency keeps its default features, so somebody else decides what is linked:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn the_http_client_links_no_decompressor_and_no_charset_decoder() {
    let entries = workspace_dependencies(&workspace_manifest());
    let ureq = entries
        .iter()
        .find(|(name, _)| name == "ureq")
        .expect("ureq is the one HTTP client");
    for forbidden in ["gzip", "brotli", "zlib", "flate", "charset"] {
        assert!(
            !ureq.1.contains(forbidden),
            "ureq must not carry `{forbidden}`: a decompressor below the body cap, or a \
             second charset decoder, is not a capability this crate asks for ({})",
            ureq.1
        );
    }
    // …and TLS stays on: a client that shipped without it would refuse https
    // rather than silently downgrade, but the feature is the thing `verifies_tls`
    // measures, so it is named here.
    assert!(
        ureq.1.contains("\"rustls\""),
        "ureq must keep rustls: {}",
        ureq.1
    );
}

/// The crates that would read a compressed body, or decode one for us.
///
/// Not content, and not a list that can go stale in a way that matters: each name
/// is an inflate/brotli implementation or a charset decoder, i.e. a parser that runs
/// on bytes somebody else chose. A new one in the graph is a new dependency edge.
const DECOMPRESSORS: &[&str] = &[
    "flate2",
    "miniz_oxide",
    "zlib-rs",
    "adler2",
    "simd-adler32",
    "crc32fast",
    "brotli-decompressor",
    "async-compression",
    "encoding_rs",
    "simdutf8",
];

/// Decompressors this workspace's binaries can link, the crate that pulls each one,
/// and why that is not a body being inflated on somebody else's terms.
///
/// This rule used to be "no decompressor anywhere in `Cargo.lock`", and it was true
/// while the product's face was a wasm page plus a fetcher. The window changed what
/// the lock can hold: `eframe` draws it, `egui-winit` gives it a clipboard, and
/// `arboard` decodes a pasted image with `image` → `png` → `flate2`. So the ban moved
/// to where it is provable (a crate that links an HTTP client links no decompressor,
/// below), and what is left is *known*: the set does not grow without a line here,
/// each line names what pulls the crate, and the desktop's own source may not name
/// one (`the_desktop_opens_no_listener_and_starts_no_process`).
///
/// The closure this is measured on is the lock's, i.e. every target's: `Cargo.lock`
/// records the dependency edges of all cfgs at once, so a backend resolved for
/// another platform counts as linked here. That is the conservative direction on
/// purpose — a rule that only counted this target would go quiet the moment somebody
/// built for the one it skipped.
const LINKED_DECOMPRESSORS: &[(&str, &str, &str)] = &[
    (
        "atlas-desktop",
        "flate2",
        "the clipboard: eframe → egui-winit → arboard → image → png → flate2 decodes a pasted PNG, a file the reader chose",
    ),
    (
        "atlas-desktop",
        "miniz_oxide",
        "the inflate backend behind that PNG decoder (flate2's default `rust_backend`)",
    ),
    (
        "atlas-desktop",
        "zlib-rs",
        "flate2's alternative backend, resolved for a target whose cfg selects it: the same clipboard decoder, another platform's build of it",
    ),
    (
        "atlas-desktop",
        "adler2",
        "the checksum flate2's inflate verifies its own output with",
    ),
    (
        "atlas-desktop",
        "simd-adler32",
        "the accelerated form of the same checksum",
    ),
    (
        "atlas-desktop",
        "crc32fast",
        "png's chunk CRC, on the same clipboard path",
    ),
    (
        "atlas-desktop",
        "simdutf8",
        "simd_cesu8, the JNI string decoder a mobile target's window stack pulls: it decodes text this build never produces",
    ),
];

#[test]
fn nothing_that_fetches_a_body_links_a_decompressor() {
    // The manifest is not enough: one transitive edge (`ureq` re-enabling `gzip`
    // through its own defaults) is how a decompressor got linked before. The lock is
    // the resolved answer — but the lock is shared, so the answer has to be per
    // binary: a decoder in the reader's clipboard cannot inflate a fetched body, and
    // pretending otherwise forces the rule to be weakened until it proves nothing.
    //
    // Three claims, in the order they can fail:
    //  1. a member that links an HTTP client links no decompressor — the original
    //     defect, stated structurally (no list of which crate fetches: a crate with
    //     a client in its closure is a crate that fetches);
    //  2. every decompressor a member *does* link is recorded, with the crate that
    //     pulls it;
    //  3. every decompressor in the lock is either linked-and-recorded or
    //     resolved-not-linked-and-recorded, so no door is left without an answer.
    let members = members(&workspace_manifest());
    let lock = lock_file();
    let mut problems: Vec<String> = Vec::new();
    let mut linked: Vec<(String, String)> = Vec::new();
    for member in &members {
        let closure = lock_closure(&lock, member);
        let clients: Vec<&str> = HTTP_CLIENTS
            .iter()
            .copied()
            .filter(|client| closure.contains(*client))
            .collect();
        let decompressors: Vec<&str> = DECOMPRESSORS
            .iter()
            .copied()
            .filter(|decoder| closure.contains(*decoder))
            .collect();
        if !clients.is_empty() && !decompressors.is_empty() {
            problems.push(format!(
                "{member} links {} and also {}: a body cap bounds bytes *read*, it cannot \
                 bound what a decompressor allocates on the way there",
                clients.join(", "),
                decompressors.join(", ")
            ));
        }
        for decoder in decompressors {
            linked.push((member.clone(), decoder.to_string()));
        }
    }
    for (member, decoder) in &linked {
        if !LINKED_DECOMPRESSORS
            .iter()
            .any(|(recorded_member, recorded, _)| recorded_member == member && recorded == decoder)
        {
            problems.push(format!(
                "{member} links `{decoder}` and no record says why: either drop it (a \
                 decoder below a body is a parser on somebody else's bytes) or add it to \
                 LINKED_DECOMPRESSORS with a sentence naming the crate that pulls it"
            ));
        }
    }
    for (member, decoder, _) in LINKED_DECOMPRESSORS {
        if !linked
            .iter()
            .any(|(linked_member, linked_decoder)| linked_member == member && linked_decoder == decoder)
        {
            problems.push(format!(
                "the record says {member} links `{decoder}`, and it does not: delete the \
                 entry (the route that pulled it is gone)"
            ));
        }
    }
    let packages = locked_packages(&lock);
    for package in &packages {
        if !DECOMPRESSORS.contains(&package.name.as_str()) {
            continue;
        }
        let explained = LINKED_DECOMPRESSORS
            .iter()
            .any(|(_, decoder, _)| *decoder == package.name);
        if !explained {
            problems.push(format!(
                "Cargo.lock names `{} {}` and no record says why it is there: name it in \
                 LINKED_DECOMPRESSORS with the crate that pulls it, or remove the \
                 dependency (a decoder below a body is a parser on somebody else's bytes)",
                package.name, package.version
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "a decompressor is linked where a body is read, or is linked without a \
         decision:\n{}",
        problems.join("\n")
    );
}

#[test]
fn no_dependency_comes_from_anywhere_but_crates_io() {
    let manifest = workspace_manifest();
    for entry in workspace_dependencies(&manifest) {
        for forbidden in ["git =", "branch =", "rev =", "tag =", "registry =", "path ="] {
            assert!(
                !entry.1.contains(forbidden),
                "{} names `{forbidden}`: a dependency comes from crates.io at a version \
                 with a checksum, or it does not come at all",
                entry.0
            );
        }
    }
    // `[patch]`/`[replace]` re-point a crate that the lock already names. A
    // *comment* that names the table is not the table (this file's own prose, and
    // the workspace manifest's, both do), so only code lines count.
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        for table in ["[patch", "[replace"] {
            assert!(
                !trimmed.starts_with(table),
                "the workspace manifest has a `{table}` table: the bytes compiled would not \
                 be the bytes Cargo.lock names"
            );
        }
    }
    let mut from_elsewhere = Vec::new();
    for package in locked_packages(&lock_file()) {
        if !package.source.is_empty() && !package.source.starts_with("registry+https://github.com/rust-lang/crates.io-index") {
            from_elsewhere.push(format!("{} {} from {}", package.name, package.version, package.source));
        }
    }
    assert!(
        from_elsewhere.is_empty(),
        "Cargo.lock has a package from another source:\n{}",
        from_elsewhere.join("\n")
    );
}

#[test]
fn no_member_carries_a_build_script() {
    let manifest = workspace_manifest();
    let members = members(&manifest);
    assert!(members.len() >= 20, "the member parser read {} members", members.len());
    let mut problems = Vec::new();
    for member in &members {
        let dir = rust_dir().join(member);
        if dir.join("build.rs").exists() {
            problems.push(format!("{member}/build.rs exists"));
        }
        let member_manifest = read(&dir.join("Cargo.toml"));
        if let Some(line) = has_build_script(&member_manifest) {
            problems.push(format!("{member}/Cargo.toml: {line}"));
        }
    }
    assert!(
        problems.is_empty(),
        "one of our crates would run code at build time:\n{}\n\
         (a build script has the user's environment and the network, and it runs before \
         anything in this workspace can be audited)",
        problems.join("\n")
    );
}

#[test]
fn the_one_release_profile_is_the_workspace_one() {
    let manifest = workspace_manifest();
    // The profile that ships: small, fast, and no unwind tables to carry a page's
    // text into a crash report.
    for (key, value) in [
        ("lto", "true"),
        ("codegen-units", "1"),
        ("panic", "\"abort\""),
        ("strip", "true"),
    ] {
        assert!(
            manifest.contains(&format!("{key} = {value}")),
            "the release profile lost `{key} = {value}`"
        );
    }
    let mut offenders = Vec::new();
    for member in members(&manifest) {
        let member_manifest = read(&rust_dir().join(&member).join("Cargo.toml"));
        for (number, line) in member_manifest.lines().enumerate() {
            if line.trim().starts_with("[profile.") {
                offenders.push(format!("{member}/Cargo.toml:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a member declares a profile, which cargo ignores (warning only), so the member \
         claims hardening the build does not apply:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn every_cargo_invocation_is_locked() {
    // The scripts are what a human runs, so they are the place the rule has to
    // hold. `cargo build --release` (no `--locked`) is allowed to re-resolve the
    // graph: versions nobody chose, binaries nobody reviewed.
    let repo = rust_dir().join("..");
    // `build-ui.sh` was on this list and is deleted (this task: it built the wasm UI,
    // and the wasm UI went with the web face). A path here must exist — `read` panics
    // rather than skips, because a script that quietly stops being checked is how a
    // `--locked` rule dies.
    let scripts = [
        rust_dir().join("build.sh"),
        repo.join("run.sh"),
        repo.join("setup.sh"),
        repo.join("run.ps1"),
        repo.join("setup.ps1"),
    ];
    let mut problems = Vec::new();
    let mut checked = 0usize;
    for script in &scripts {
        let text = read(script);
        let name = script.file_name().unwrap().to_string_lossy().to_string();
        let mut offline = false;
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // A comment describes a command; it does not run one.
            if trimmed.starts_with('#') {
                continue;
            }
            // A *sentence* that names the command (a `Die "cargo build failed…"`
            // line) is not an invocation: every invocation in these scripts asks
            // for a release build, and no error message does.
            let invokes = trimmed.contains("--release")
                && (trimmed.contains("cargo build")
                    || trimmed.contains("cargo run")
                    || trimmed.contains("cargo install"));
            if !invokes {
                continue;
            }
            checked += 1;
            if trimmed.contains("--offline") {
                offline = true;
            }
            if !trimmed.contains("--locked") {
                problems.push(format!("{name}:{}: {}", number + 1, trimmed));
            }
        }
        assert!(
            offline || !name.starts_with("build"),
            "{name} never asks for an offline build"
        );
    }
    assert!(checked >= 10, "the scanner saw only {checked} cargo invocations");
    assert!(
        problems.is_empty(),
        "a build script can re-resolve the dependency graph:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_lock_holds_a_checksum_for_everything_it_names() {
    let lock = lock_file();
    assert!(
        lock.contains("version = 4"),
        "the lock format changed: `--locked` reasoning in rust/Cargo.toml assumes it"
    );
    let packages = locked_packages(&lock);
    assert!(packages.len() > 100, "the lock parser read {} packages", packages.len());
    let mut problems = Vec::new();
    for package in &packages {
        if package.source.is_empty() {
            // A workspace member: no source, no checksum, nothing to verify.
            continue;
        }
        if !package.checksum {
            problems.push(format!("{} {} has no checksum", package.name, package.version));
        }
    }
    assert!(
        problems.is_empty(),
        "a crate in the lock cannot be verified:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_manifest_and_the_lock_name_the_same_versions() {
    // This is the claim `--locked` enforces at build time, checked without cargo:
    // if these disagree, every build from this tree either fails or resolves.
    let entries = workspace_dependencies(&workspace_manifest());
    let packages = locked_packages(&lock_file());
    let mut wrong = Vec::new();
    for (name, value) in &entries {
        let Some(version) = declared_version(value) else {
            wrong.push(format!("{name}: no version"));
            continue;
        };
        let wanted = version.trim_start_matches('=');
        let found = packages
            .iter()
            .any(|package| package.name == *name && package.version == wanted);
        if !found {
            wrong.push(format!(
                "{name}: the manifest says {wanted}, Cargo.lock does not"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the manifest and the lock disagree (run `cargo build --offline` once to rewrite \
         the lock, then commit it):\n{}",
        wrong.join("\n")
    );
}

/// Every crate in this graph that runs code at **build time**, measured with:
///
/// ```text
/// cd rust
/// cargo tree --offline --prefix none -e normal,build \
///   | awk '{v=$2; sub(/^v/,"",v); print $1"-"v}' | sort -u
/// ```
/// and then asking the crate cache of each of those whether it has a `build.rs`.
///
/// Why this is a security list and not bookkeeping: a build script runs on the
/// user's machine, with the user's environment, before a single line of this
/// workspace is audited, and nothing stops it opening a socket. Thirty-one is not
/// zero, so the answer cannot be "trust them" — it is that the set is *known*, it
/// does not grow without a diff here, and what each one does is written down.
///
/// The set grew by thirteen when the face of the product became a window: a
/// toolkit that talks to the platform's graphics server and input stack
/// (`winit`, `glutin`, `glutin-winit`, `objc-sys`, `objc2`, `rustix`) carries
/// `cfg` probes and generated shims, and the groups below are what they are.
///
/// * `cfg` aliases only — a `cfg_aliases!` block, no probe, no compiler:
///   `glutin`, `glutin-winit`, `winit`;
/// * version/cfg probes, no network, no compiler: `serde`, `serde_core`,
///   `serde_json`, `thiserror`, `httparse`, `proc-macro2`, `quote`, `libc`,
///   `getrandom`, `rustls`, `zmij`, `wasm-bindgen`, `wasm-bindgen-shared`,
///   `ahash`, `crossbeam-utils`, `num-traits`, `parking_lot_core`,
///   `zerocopy`, `objc2` (which runtime is linked is a target question);
///   `memoffset` left this group with `eframe` 0.37, which no longer resolves it;
/// * the Objective-C runtime the window binds to: `objc-sys`, which reads the
///   target and picks Apple's runtime, GNUStep or WinObjC;
/// * generated source written into `OUT_DIR`: `icu_normalizer_data`,
///   `icu_properties_data`, `rustix` (its `linux_raw` inline-asm shims) and
///   `unicode-general-category` (it compiles the general-category table in its own
///   `src/tables.rs` into `category.rs` — a font-metric dependency of `epaint` 0.37,
///   so it arrived with the window toolkit, not with anything that fetches);
/// * a `rustc --version` subprocess: `crc32fast`, which asks the compiler it is
///   already being compiled by whether `aarch64` has the CRC32 intrinsic;
/// * a C compiler: `libsqlite3-sys` (the bundled `sqlite3.c`) and `ring` (its
///   assembly). Both read `CC`/`CFLAGS`-shaped environment variables, which is the
///   one place a hostile environment could reach the compiled bytes — worth
///   knowing, and out of reach of a `build.rs`-free manifest.
///
/// * a linker directive for a platform library: `glutin_egl_sys`, `glutin_wgl_sys`
///   and `windows_x86_64_msvc` each emit `cargo:rustc-link-lib`/`link-search` for
///   `libEGL`, `opengl32` and the MSVC import libraries — they arrived with
///   `glutin` (the window toolkit's Windows and EGL backends) and `windows-sys`,
///   and they only exist in the resolve for those targets. `khronos_api` is the
///   Khronos XML those crates read at build time. None of the four fetches
///   anything: each reads files that ship inside its own crate and writes link
///   metadata.
///
/// `encoding_rs` and `crc32fast` were on this list until `rust/Cargo.toml` stopped
/// asking ureq for `gzip` + `charset`; two fewer crates run code at build time
/// than before that change. The four above appeared when a Windows target was
/// fetched for a cross-check (`cargo fetch --target x86_64-pc-windows-msvc`): they
/// are recorded rather than excluded, because a build script is a build script on
/// every target.
const BUILD_TIME_CRATES: &[&str] = &[
    "ahash",
    // `cookie` 0.18.2 — pulled by `wry` (the in-process web engine). Its build script
    // asks `version_check` whether this compiler knows `doc_cfg` and sets
    // `cargo:rustc-cfg=nightly` when it does. A compiler probe: it opens no socket,
    // starts no process and fetches nothing.
    "cookie",
    "crc32fast",
    "crossbeam-utils",
    "getrandom",
    "glutin",
    "glutin-winit",
    "glutin_egl_sys",
    "glutin_wgl_sys",
    "httparse",
    "icu_normalizer_data",
    "khronos_api",
    "icu_properties_data",
    "libc",
    "libsqlite3-sys",
    "num-traits",
    "objc-sys",
    "objc2",
    // `objc2-exception-helper` 0.1.1 — pulled by `wry` on macOS: its build script compiles
    // one Objective-C file (`src/try_catch.m`, a `try`/`catch` shim) with `cc`, no ARC,
    // into `OUT_DIR`. Compiling a local source file: no socket, no process at run time,
    // nothing fetched.
    "objc2-exception-helper",
    "parking_lot_core",
    "proc-macro2",
    "quote",
    "ring",
    "rustix",
    "rustls",
    "serde",
    "serde_core",
    "serde_json",
    "thiserror",
    "unicode-general-category",
    "wasm-bindgen",
    "wasm-bindgen-shared",
    "windows_x86_64_msvc",
    "winit",
    // `wry` 0.57.3 — the in-process system web engine (WKWebView on macOS, WebView2 on
    // Windows; not compiled on Linux). Its build script emits
    // `cargo:rustc-link-lib=framework=WebKit` on macOS/iOS and generates Kotlin source on
    // Android. Linker directives and generated text: the engine itself is the operating
    // system's, and this script opens no socket, starts no process and fetches nothing.
    "wry",
    // `webview2-com-sys` 0.39.1 — wry's Windows half, the WebView2 loader. `cfg`-gated to
    // `target_os = "windows"` by wry, so a macOS or Linux build never compiles it; it is on
    // this list because the rule reads the crate cache, which is the set a Windows build
    // would run. Its build script copies the three `WebView2Loader` binaries that ship
    // inside the crate (`webview2-com-sys/{arm64,x64,x86}/…`) into `OUT_DIR` and emits
    // `cargo:rustc-link-lib=advapi32`: local file copies and one linker directive, no
    // socket, no process, nothing fetched over the network.
    "webview2-com-sys",
    "zerocopy",
    "zmij",
];

/// `$CARGO_HOME/registry/src/index.crates.io-*`, the extracted crates this machine
/// can compile. `None` when this is a vendored build or a bare checkout.
fn registry_source() -> Option<PathBuf> {
    let cargo_home = match std::env::var("CARGO_HOME") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".cargo"),
    };
    let src = cargo_home.join("registry").join("src");
    for entry in std::fs::read_dir(src).ok()? {
        let path = entry.ok()?.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        if path.is_dir() && name.starts_with("index.crates.io-") {
            return Some(path);
        }
    }
    None
}

#[test]
fn the_crates_that_run_code_at_build_time_are_the_recorded_ones() {
    // A crate Cargo.lock names but this machine never fetched cannot be compiled
    // here, so the cache is exactly the set a build could run. One package may be
    // present in several versions (`getrandom` 0.2 and 0.4 are both in the lock);
    // the answer is per *name*, because the list is per name.
    let Some(registry) = registry_source() else {
        eprintln!("audit_manifest: no crate cache under $CARGO_HOME — build-time set unverified");
        return;
    };
    let mut cached: std::collections::BTreeMap<String, (bool, String)> = Default::default();
    let packages = locked_packages(&lock_file());
    for package in &packages {
        if package.source.is_empty() {
            continue;
        }
        let dir = registry.join(format!("{}-{}", package.name, package.version));
        if !dir.is_dir() {
            continue;
        }
        let has_script = dir.join("build.rs").is_file();
        let entry = cached
            .entry(package.name.clone())
            .or_insert((false, package.version.clone()));
        entry.0 = entry.0 || has_script;
    }
    let mut unexpected = Vec::new();
    for (name, (has_script, version)) in &cached {
        if *has_script && !BUILD_TIME_CRATES.contains(&name.as_str()) {
            unexpected.push(format!("{name} {version}"));
        }
    }
    assert!(
        unexpected.is_empty(),
        "a crate runs code at build time and is not on the list — read its build.rs, then \
         add it here with a sentence saying what it does:\n{}",
        unexpected.join("\n")
    );
    let mut stale = Vec::new();
    for name in BUILD_TIME_CRATES {
        match cached.get(*name) {
            Some((has_script, version)) if !*has_script => {
                stale.push(format!("{name} {version} has no build.rs"))
            }
            Some(_) => {}
            // Not in the lock at all any more: the list kept a memory of a crate
            // this graph no longer has.
            None => stale.push(format!("{name} is not in Cargo.lock")),
        }
    }
    assert!(
        stale.is_empty(),
        "the build-time list is stale (measure it again with the command in the comment):\n{}",
        stale.join("\n")
    );
}

// ---------------------------------------------------------------------------
// a path a compiler, a browser and a URL can all name
// ---------------------------------------------------------------------------

/// The tree ships no file whose name holds a space.
///
/// This is not a style rule about our taste: a space is what a sync conflict or a
/// bulk copy adds to the name of a file it could not overwrite (`state 2.rs`,
/// `public/style 3.css`, `tiles/6/61/13 2.png`), and each of those copies has cost
/// this project a real defect — a copy of the log writer that the audit blessed as
/// the writer, a copy of the monitors view that still contained the line an XSS
/// pass had just fixed, a stale second `sections.css` nothing loaded, 13 000 dead
/// tiles. Two of them also broke cargo's own target discovery ("invalid character
/// ' ' in crate name"), i.e. a stray copy can stop the whole test suite.
///
/// So the rule is mechanical, and it covers the tree a URL serves as well as the
/// tree rustc reads: a name with a space in it cannot be a `mod` name and cannot be
/// served (the static policy refuses a space), which makes every such file dead
/// weight at best and a decoy at worst.
///
/// `data/` and `target/` are skipped: the first is the reader's archive (its files
/// are named by the app and by SQLite, not by us) and the second is generated.
#[test]
fn no_shipped_file_is_named_with_a_space() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|rust| rust.parent())
        .expect("the crate lives at rust/atlas-guard")
        .to_path_buf();
    let mut found: Vec<String> = Vec::new();
    let mut read = 0usize;
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable directory").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                // Not ours to judge: the reader's archive and the build output.
                let skip = ["target", "data", ".git", "node_modules"]
                    .iter()
                    .any(|skip| name == *skip);
                if !skip {
                    stack.push(path);
                }
                read += 1;
                continue;
            }
            read += 1;
            if name.contains(' ') {
                let shown = path
                    .strip_prefix(&root)
                    .map(|rel| rel.display().to_string())
                    .unwrap_or_else(|_| name.clone());
                found.push(shown);
            }
        }
    }
    // A scan that read nothing proves nothing; the tree has hundreds of files.
    assert!(
        read > 500,
        "the walk read only {read} entries — the root is wrong, so a green result means nothing"
    );
    assert!(
        found.is_empty(),
        "a file name holds a space, so it is a copy nothing can compile and no URL can \
         serve — delete it (and, if it is the only copy, rename it into place):\n{}",
        found.join("\n")
    );
}

// ---------------------------------------------------------------------------
// the face is a window: no client, no socket, no spawn, a read-only archive
// ---------------------------------------------------------------------------
//
// The product used to be a wasm page served by a listener in this process, so the
// build rules above were about what a *server* links. The face is a native window
// now (`rust/atlas-desktop`: egui/eframe, rusqlite, serde_json), and the claim it
// makes is one sentence long — nothing leaves this machine. A claim like that is
// made of four build-time facts, and every one of them is checkable without
// opening the window:
//
// 1. the binary links no HTTP client (below);
// 2. the one connection it opens cannot write the archive;
// 3. it opens no listener, and starts no process — and the one *client* socket it
//    does open (the chat view talks to the local model server) is confined to one
//    recorded file and to a literal loopback address, so no name is resolved and
//    nothing can be addressed off this machine;
// 4. it is a workspace member whose four pins are exact, like every other crate.
//
// Facts 2 and 3 are read off the shipped source with `atlas_guard::source`, the
// same scanner `audit_binds`, `audit_logs` and `audit_downloads` use, because the
// question "could this line run" has one answer in this workspace: comments are
// dropped by the matcher (a doc line that *describes* `Command::new` is prose), a
// `#[cfg(test)]` block is blanked (a test that names a primitive is not shipped
// code), and a needle is matched ignoring whitespace across line breaks (`Tcp
// Listener::bind` is `TcpListener::bind`). Grepping with `str::contains` gets all
// three of those wrong in the silent direction.

/// Where the desktop crate's shipped source lives, relative to `rust/`.
const DESKTOP_SRC: &str = "atlas-desktop/src/";

/// Every shipped `.rs` of `atlas-desktop`.
fn desktop_files() -> Vec<SourceFile> {
    let files: Vec<SourceFile> = shipped_code(&rust_dir())
        .into_iter()
        .filter(|file| file.path.starts_with(DESKTOP_SRC))
        .collect();
    // A scan that read nothing proves nothing, and this crate has 13 files.
    assert!(
        files.len() >= 10,
        "the scanner read only {} files under {DESKTOP_SRC} — a green result below \
         would mean nothing",
        files.len()
    );
    files
}

/// One `[[package]]` block's dependency list of `Cargo.lock`: the package name and
/// every name it depends on.
///
/// `dependencies = [ … ]` is always one entry per line in this lock, and an entry
/// may carry the version the resolve chose (`"bitflags 2.13.2",`), so the name is
/// the first token. Read as an edge list rather than a name search because the
/// lock is shared: the crate that *must* not link a client and the crates that
/// must are in the same file.
fn lock_package_edges(lock: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut name = String::new();
    let mut deps: Vec<String> = Vec::new();
    let mut in_deps = false;
    let mut in_block = false;
    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            if in_block && !name.is_empty() {
                out.push((std::mem::take(&mut name), std::mem::take(&mut deps)));
            }
            in_block = true;
            in_deps = false;
            continue;
        }
        if !in_block {
            continue;
        }
        if in_deps {
            if trimmed.starts_with(']') {
                in_deps = false;
                continue;
            }
            let token = trimmed
                .trim_matches(|ch: char| ch == '"' || ch == ',' || ch.is_whitespace())
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !token.is_empty() {
                deps.push(token.to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("dependencies = [") {
            in_deps = !rest.contains(']');
            for piece in rest.split(']').next().unwrap_or("").split(',') {
                let token = piece
                    .trim()
                    .trim_matches('"')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if !token.is_empty() {
                    deps.push(token.to_string());
                }
            }
            continue;
        }
        if name.is_empty() {
            if let Some(rest) = trimmed.strip_prefix("name = ") {
                name = rest.trim_matches('"').to_string();
            }
        }
    }
    if !name.is_empty() {
        out.push((name, deps));
    }
    out
}

/// Every crate the named package is compiled against, by name: the transitive
/// closure of its `dependencies` edges in `Cargo.lock`.
fn lock_closure(lock: &str, root: &str) -> std::collections::BTreeSet<String> {
    let mut edges: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (name, deps) in lock_package_edges(lock) {
        edges.entry(name).or_default().extend(deps);
    }
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut queue = vec![root.to_string()];
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for dep in edges.get(&name).into_iter().flatten() {
            if !seen.contains(dep) {
                queue.push(dep.clone());
            }
        }
    }
    seen
}

/// The crates that speak HTTP, or listen for it — a socket by construction.
///
/// Not content and not a publisher list: each name is a client or a server, i.e. the
/// code that opens the socket a review of `src/` cannot see, and a new one in the
/// graph is a new dependency edge (a diff in a `Cargo.toml`) rather than something
/// that can arrive unnoticed. Two rules read this list: the window links no client
/// (`the_desktop_links_no_http_client`), and a crate that links a client links no
/// decompressor (`nothing_that_fetches_a_body_links_a_decompressor`).
const HTTP_CLIENTS: &[&str] = &[
    "ureq",
    "reqwest",
    "hyper",
    "hyper-util",
    "h2",
    "isahc",
    "attohttpc",
    "minreq",
    "curl",
    "curl-sys",
    "surf",
    "awc",
    "tungstenite",
    "tokio-tungstenite",
    "tiny_http",
    "axum",
    "warp",
    "actix-web",
    "actix-http",
    "rocket",
    "tide",
];

#[test]
fn the_desktop_links_no_http_client() {
    // A dependency *is* the code that would do it: a client linked for a hyperlink
    // handler, a remote font, an avatar or one line of "check for updates" is a
    // socket in the reader's binary, and no review of `src/` would show it.
    //
    // The address of the check is the *closure*, not the lock: `ureq`, `hyper`, `h2`
    // and `tokio` are in this lock for `atlas-net` and the retired server, and if
    // this test searched the whole file it would have to be weakened until it proved
    // nothing about the window. The window speaks HTTP — `src/http.rs` writes one
    // request line by hand to the local model — which is exactly why the crate
    // dependency must stay empty: a client crate would bring redirects, TLS, proxy
    // environment variables and connection pooling to a binary that needs one POST to
    // `127.0.0.1`.
    let lock = lock_file();
    let packages = locked_packages(&lock);
    let closure = lock_closure(&lock, "atlas-desktop");
    // The canaries, each *derived* from the two files this rule reads, so none of them
    // can go stale the way a remembered crate name does (this test used to insist
    // `webbrowser` was in the closure; eframe 0.37 put it behind a `egui-winit` feature
    // this workspace turns off, and the canary outlived the world it described).
    //
    // (i) every dependency `atlas-desktop/Cargo.toml` declares must be in the closure.
    //     This is the walk's floor: it fails if the parser stops reading
    //     `dependencies = [ … ]` blocks, if the walk stops starting at the crate, or if
    //     a declared dependency is renamed out from under it.
    let declared: Vec<String> = desktop_dependencies(&read(
        &rust_dir().join("atlas-desktop").join("Cargo.toml"),
    ))
    .into_iter()
    .map(|(name, _)| name)
    .collect();
    assert!(
        declared.len() >= 4,
        "the parser read only {} dependencies out of atlas-desktop/Cargo.toml",
        declared.len()
    );
    for dependency in &declared {
        assert!(
            closure.contains(dependency),
            "`{dependency}` is declared in atlas-desktop/Cargo.toml and missing from the \
             closure: the walk is not following the lock's edges"
        );
    }
    // (ii) two crates that are strictly *transitive* — reachable only through a
    //     dependency's own subtree — must be there too: `winit` below `eframe` (the
    //     event loop a window cannot be built without) and `libsqlite3-sys` below
    //     `rusqlite`'s `bundled` feature (the SQLite the archive is read with). A walk
    //     that stopped at depth one passes (i) and fails here; and a window that swapped
    //     its event loop or its SQLite binding is a manifest change that should make a
    //     reader look, not something this list can absorb silently.
    for (deep, through) in [("winit", "eframe"), ("libsqlite3-sys", "rusqlite")] {
        assert!(
            closure.contains(deep),
            "`{deep}` is not in the closure: it arrives through `{through}`, so either the \
             walk is shallow or `{through}` no longer brings it — read the manifest before \
             changing this line"
        );
    }
    // …and the walk has to have gone somewhere: four declared dependencies, and the
    // closure is measured at 110 crates (`cargo tree --offline --locked -p atlas-desktop`).
    assert!(
        closure.len() > declared.len() * 10,
        "the closure is {} crates for {} declared dependencies — a walk that stopped at \
         depth one would look exactly like this",
        closure.len(),
        declared.len()
    );
    // (iii) it is a strict *slice* of the lock, and the lock still names at least one
    //     HTTP client. The first half says the walk started at the crate; the second is
    //     the contrast that makes "this binary links none" a statement instead of a
    //     vacuum — if every client left the workspace, this rule would be green about
    //     nothing and the message says so.
    assert!(
        closure.len() < packages.len(),
        "the closure is {} crates and the lock is {}: the walk did not start at \
         atlas-desktop",
        closure.len(),
        packages.len()
    );
    let clients_in_the_lock: Vec<&str> = HTTP_CLIENTS
        .iter()
        .copied()
        .filter(|client| packages.iter().any(|package| package.name == *client))
        .collect();
    assert!(
        !clients_in_the_lock.is_empty(),
        "the lock names no HTTP client at all ({}), so \"atlas-desktop links none\" proves \
         nothing — re-state this rule against what the workspace actually fetches with",
        HTTP_CLIENTS.join(", ")
    );
    let found: Vec<String> = HTTP_CLIENTS
        .iter()
        .filter(|client| closure.contains(**client))
        .map(|client| (*client).to_string())
        .collect();
    assert!(
        found.is_empty(),
        "atlas-desktop links an HTTP client: {}\n\
         fix: drop the dependency and use `atlas-net`'s fetcher from the process that \
         already runs (the window reads `data/atlas.db` and draws, it fetches nothing) — \
         a window that can open a socket is a claim about our own `src/` only",
        found.join(", ")
    );
}


#[test]
fn the_desktop_opens_no_listener_and_starts_no_process() {
    // The old face was a server: one process, a port, a wasm page. The new one is a
    // window that reads a file, and these are the primitives that would make it
    // something else — a bind (the server, back again, on whatever interface), a
    // spawn (the shell, reachable from text the window is merely *showing*), and a
    // name lookup (the one step that turns "this machine" into "somewhere").
    //
    // The needles are the primitives, spelled as code: `TcpListener`/`UdpSocket`/
    // `UnixListener` because the import may be renamed and a listen socket is the
    // thing that is banned outright, `Command::new` and `process::Command` because
    // that is how a process starts, `webbrowser::` because that is the crate an
    // `OpenUrl` handler spawns through (it is not in this lock today — eframe 0.37 gates
    // it behind a feature this workspace turns off — so the needle is the tripwire for
    // it coming back, not a description of the tree), and `to_socket_addrs`/
    // `lookup_host`/`ToSocketAddrs` because resolving a name is a request leaving this
    // machine before any byte is written.
    const PRIMITIVES: &[&str] = &[
        "TcpListener",
        "UdpSocket",
        "UnixListener",
        "Command::new",
        "process::Command",
        "webbrowser::",
        "to_socket_addrs",
        "lookup_host",
        "ToSocketAddrs",
        "SOCK_STREAM",
    ];
    // The exception, and the only one: `src/http.rs` connects to the local model
    // server. A connect is not a bind — nothing in this process is reachable — and
    // the file it lives in is the *recorded* place a socket may appear, which is what
    // makes the ban above a statement about the crate instead of about one function.
    //
    // What keeps that connect honest is the address rule inside the same file, and the
    // audit asserts the rule, not the intent: `endpoint()` accepts an `http://` URL
    // whose host is the literal `127.0.0.1` and whose port is a number, and it refuses
    // everything else — `https://`, `ftp://`, a bare host, `localhost`, `[::1]`,
    // userinfo, `10.1.2.3` (its own tests at `http.rs:470` list those refusals). With
    // no name accepted there is nothing to resolve, so `ATLAS_MODEL_URL` — an
    // environment variable — can only pick a port on this machine.
    const CLIENT_SOCKETS: &str = "atlas-desktop/src/http.rs";
    let files = desktop_files();
    let mut found: Vec<String> = Vec::new();
    // Three lines of span: `TcpListener ::` broken before `bind` is the same call,
    // and pressing Enter is the cheapest bypass there is.
    for needle in PRIMITIVES {
        for (path, line, function, text) in find_wrapped(&files, needle, 3) {
            found.push(format!("{path}:{line} in {function}(): {text}"));
        }
    }
    // `TcpStream`/`std::net` are allowed in the recorded file and nowhere else, so a
    // second socket has to be a diff in this list.
    const STREAM_NEEDLES: &[&str] = &["TcpStream", "std::net", "SocketAddr"];
    for needle in STREAM_NEEDLES {
        for (path, line, function, text) in find_wrapped(&files, needle, 3) {
            if path == CLIENT_SOCKETS {
                continue;
            }
            found.push(format!(
                "{path}:{line} in {function}(): {text} — a socket outside                  `{CLIENT_SOCKETS}`, which is the one recorded client"
            ));
        }
    }
    // The address rule, asserted where the connect is: it must be a literal loopback,
    // and no name may be resolved anywhere in the crate.
    let http = files
        .iter()
        .find(|file| file.path == CLIENT_SOCKETS)
        .expect("atlas-desktop/src/http.rs is the recorded place a socket may appear");
    assert!(
        !find_wrapped(std::slice::from_ref(http), "127.0.0.1", 2).is_empty(),
        "`{CLIENT_SOCKETS}` no longer names `127.0.0.1`: the window's only socket must          connect to a loopback literal, because a name is an address this machine cannot          vouch for"
    );
    assert!(
        !find_wrapped(std::slice::from_ref(http), "SocketAddr::from", 2).is_empty(),
        "`{CLIENT_SOCKETS}` no longer builds its address from octets          (`SocketAddr::from(([127, 0, 0, 1], port))`): parsing a string into an address is          how a host name gets resolved later"
    );
    // The way in that has no primitive in it at all: egui's link widgets, which raise
    // `OpenUrl` and let the platform opener run whatever the URL names. Banned outright
    // now, because the window has none: the reading pane's link became a *Copy the link*
    // button (`views/articles.rs` asks `link::openable`, then calls `ctx.copy_text`), the
    // address rule is asserted at that site below, and eframe 0.37 with
    // `default-features = false` does not link `webbrowser` at all. A hit here is a URL
    // out of the archive on its way to the shell with no gate in between — the strongest
    // form of this rule, not a recorded exception to it.
    for needle in ["hyperlink_to", "open_url(", "Hyperlink::from_label_and_url"] {
        for (path, line, function, text) in find_wrapped(&files, needle, 3) {
            found.push(format!(
                "{path}:{line} in {function}(): {text} — a click here raises `OpenUrl`, which \
                 the platform hands to the OS opener with a URL the archive supplied"
            ));
        }
    }
    // A decoder in this crate would be the one way the clipboard's `flate2` could be
    // pointed at a body `src/http.rs` read: the crate may link it (eframe's clipboard
    // does) and the shipped source may not name it.
    for needle in ["flate2", "GzDecoder", "ZlibDecoder", "miniz_oxide", "crc32fast"] {
        for (path, line, _, text) in find_wrapped(&files, needle, 3) {
            found.push(format!(
                "{path}:{line}: {text} — the window inflates nothing: a decoder here would                  run on bytes somebody else chose"
            ));
        }
    }
    assert!(
        found.is_empty(),
        "atlas-desktop can open a listen socket, resolve a name, start a process or          inflate a body:\n{}\n\
         fix: the window reads `data/atlas.db` and draws, and its one socket is a POST to          the model on 127.0.0.1 (`src/http.rs`). A listener belongs to no binary in this          product; a spawn opens the reader's browser or shell with content the window did          not write — copy the text instead of running something on it",
        found.join("\n")
    );
    // …and the *replacement* has to stay what it is, or the ban above is the only thing
    // between an archive row and the shell. Two halves, both measured on the shipped text:
    // the gate exists (`link::openable` judges an address and `link::label` shows where it
    // goes), and the one site that handles a stored address *copies* it instead of opening
    // it. If someone puts a hyperlink back, the scan above catches the call and this
    // catches the removal of the copy.
    let gate = files
        .iter()
        .find(|file| file.path == "atlas-desktop/src/link.rs")
        .expect("atlas-desktop/src/link.rs is the one place a stored address is judged");
    assert!(
        gate.code.contains("pub fn openable") && gate.code.contains("pub fn label"),
        "`atlas-desktop/src/link.rs` no longer offers `openable`/`label`: the reading pane's \
         link would have no gate and no visible destination"
    );
    let pane = files
        .iter()
        .find(|file| file.path == "atlas-desktop/src/views/articles.rs")
        .expect("atlas-desktop/src/views/articles.rs is where a stored address is shown");
    for call in ["crate::link::openable", "crate::link::label", "copy_text"] {
        assert!(
            pane.code.contains(call),
            "`atlas-desktop/src/views/articles.rs` does not call `{call}`: the address a \
             reader can act on has to be judged, shown, and copied — never handed to the \
             platform opener"
        );
    }
}

#[test]
fn the_desktop_opens_the_archive_read_only_and_writes_nowhere() {
    // Two independent locks, because they fail differently. `SQLITE_OPEN_READ_ONLY`
    // is the loader's promise: it holds even if a view is wrong, and it is what stops
    // a concurrent writer's file from being opened by a second writer. "No write
    // statement in `atlas-desktop/src`" is the promise that survives a later
    // `OpenFlags` edit made for a feature — a view that means well ("mark as read")
    // cannot land, because there is no code path that writes.
    let files = desktop_files();
    let data = files
        .iter()
        .find(|file| file.path.ends_with("/data.rs"))
        .expect("atlas-desktop/src/data.rs is the one place the archive is opened");
    for needle in [
        "Connection::open_with_flags",
        "OpenFlags::SQLITE_OPEN_READ_ONLY",
    ] {
        assert!(
            !find_wrapped(std::slice::from_ref(data), needle, 3).is_empty(),
            "the desktop no longer opens the archive with `{needle}`: `data.rs` must open \
             one connection, read-only, or the window can write the archive the engines own"
        );
    }
    // The second lock, and the one that looks like a no-op when it is got wrong:
    // `PRAGMA query_only` with no argument *reports* the switch — it answers the
    // integer `0` — so it has to be set (`pragma_update(None, "query_only", true)`)
    // and then read back. Written as `let _: String = query_row("PRAGMA query_only")`
    // it was neither: rusqlite answers a non-Text column with `InvalidColumnType`,
    // so `open()` returned `Err` on every machine. The needle is the whole call,
    // value included, because "query_only" in a doc comment is a sentence while
    // `pragma_update` is the switch.
    //
    // The open used to carry `OpenFlags::SQLITE_OPEN_URI` as well, and no longer does:
    // that is the *stricter* direction (with URI mode off a path is a path, and
    // `file:…?mode=rw` in a filename is not a flag), so the needle went away rather
    // than the rule bending. What is asserted is the flag that is load-bearing, the
    // switch being *set*, and the read-back that refuses a connection which did not
    // take it.
    for needle in [
        "pragma_update(None, \"query_only\", true)",
        // The read-back: `data.rs` reads the pragma after setting it and returns an
        // error when it answers 0. Without it, a driver that ignored the setting would
        // be a writable connection nobody noticed.
        "PRAGMA query_only",
    ] {
        assert!(
            !find_wrapped(std::slice::from_ref(data), needle, 3).is_empty(),
            "`data.rs` no longer holds `{needle}`: the connection is read-only by two \
             locks — the open flag, and `query_only` set (then read back) rather than \
             assumed — and a view that could write the archive has to be impossible, not \
             merely unlikely"
        );
    }
    // `Connection::open` is read-write **and creates the file** — the exact opposite
    // of the claim, and one word away from the correct call.
    let bare = find_wrapped(&files, "Connection::open(", 2);
    assert!(
        bare.is_empty(),
        "the desktop opens a connection read-write:\n{}\n\
         fix: `Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)`",
        bare.iter()
            .map(|(path, line, _, text)| format!("{path}:{line}: {text}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // A read-only connection is the lock; these two flags would remove it.
    //
    // The one file that may name them is the queue (`src/requests.rs`): the window's
    // single write target is `data/requests.db`, a second database holding *intent
    // rows*, not the archive. That file is checked from the other side — it must name
    // `requests.db` and must not name `atlas.db` — so the flag can never be moved onto
    // the archive's connection.
    const QUEUE: &str = "atlas-desktop/src/requests.rs";
    let not_the_queue: Vec<SourceFile> = files
        .iter()
        .filter(|file| !file.path.ends_with(QUEUE))
        .cloned()
        .collect();
    for needle in ["SQLITE_OPEN_READ_WRITE", "SQLITE_OPEN_CREATE"] {
        let hits = find_wrapped(&not_the_queue, needle, 3);
        assert!(
            hits.is_empty(),
            "atlas-desktop names `{needle}` outside the queue: the archive's connection is \
             opened `SQLITE_OPEN_READ_ONLY` and nothing else, so a view cannot write the \
             archive or create one beside it:\n{}",
            hits.iter()
                .map(|(path, line, _, text)| format!("{path}:{line}: {text}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let queue = files
        .iter()
        .find(|file| file.path.ends_with(QUEUE))
        .expect("atlas-desktop/src/requests.rs is the window's queue");
    // The needles are the *quoted* names, because a path this connection can open is a
    // string literal while a sentence about a path is prose: `requests.rs` explains in a
    // comment (measured: line 281) that redacting a `data/atlas.db` path as a value removed
    // real URLs, and an unquoted needle read that note as a second file. The rule is about
    // what the code opens, so it asks for the literal.
    assert!(
        queue.code.contains("\"requests.db\"") && !queue.code.contains("\"atlas.db\""),
        "the window's read-write connection is not the queue: `{QUEUE}` must open exactly \
         `data/requests.db` and never the archive"
    );
    // And the statements: `execute`/`execute_batch` are how rusqlite runs a
    // statement, and every query this crate makes goes through `query_row`/`prepare`.
    // Both a verb and an API, because either one alone is a way around. Again scoped to
    // every file but the queue, whose own `CREATE TABLE`/`INSERT` for `requests.db` is
    // the intent write the agent reads — its table list is pinned against
    // `atlas-store`'s by `src/requests.rs`'s own test.
    for needle in [
        ".execute(",
        "execute_batch(",
        "INSERT INTO",
        "DELETE FROM",
        "CREATE TABLE",
        "DROP TABLE",
        "ALTER TABLE",
        "REPLACE INTO",
        "ATTACH DATABASE",
        "VACUUM",
    ] {
        let hits = find_wrapped(&not_the_queue, needle, 3);
        assert!(
            hits.is_empty(),
            "atlas-desktop carries a write (`{needle}`):\n{}\n\
             fix: the window reads. Writing the archive is `atlas-store`'s, under the one \
             connection the engines serialize — a write here races them",
            hits.iter()
                .map(|(path, line, _, text)| format!("{path}:{line}: {text}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// The window ships no filesystem writer.
///
/// The test above is the promise about the **archive**: opened `SQLITE_OPEN_READ_ONLY`,
/// `PRAGMA query_only` set and read back, and no write statement anywhere in the crate.
/// This is the promise about every *other* file, and it is the one that had no test —
/// the archive is covered because a second writer would race the engines, but a file the
/// window creates itself is a different mistake: `src/main.rs` says the window is "a
/// window, a GPU surface and a database", and the one directory ATLAS owns is the data
/// directory, whose rules live in `atlas-config`'s `Paths` and `atlas-platform`'s
/// private-file helpers (0600 from the first byte, no symlink followed, atomic rename).
/// A writer here would be state kept somewhere nobody audited: a graph layout cached on
/// disk, a saved window size, a line of debug log, a screenshot of the page.
///
/// Measured when this test was written: zero hits for every needle below in
/// `atlas-desktop/src`, with test modules excluded (`atlas_guard::source::shipped_code`
/// drops `tests.rs`, `*_tests.rs` and files a crate declares `#[cfg(test)] mod x;`). The
/// only `write` in the crate is `TcpStream::write_all` in `src/http.rs`, which writes a
/// socket, not a file — that is why the needles are the filesystem API and not the word.
#[test]
fn the_desktop_writes_no_file() {
    // Both spellings of each primitive: `std::fs::write` and a `use std::fs;` alias,
    // `File::create` and `File::options().create(true)`, and `OpenOptions` at all
    // (its only purpose is to open for writing). `append(`, `truncate(` and `create(`
    // are here because an `OpenOptions` assembled in a helper is still a writer.
    const WRITERS: &[&str] = &[
        "fs::write(",
        "fs::rename(",
        "fs::remove_file(",
        "fs::remove_dir_all(",
        "fs::create_dir(",
        "create_dir_all(",
        "fs::set_permissions(",
        "File::create",
        "File::append",
        "OpenOptions",
        ".create(true)",
        ".append(true)",
        ".truncate(true)",
    ];
    // The one exemption, and it is about the *queue*, not about writing: the window's
    // single write target is `data/requests.db` (the intent rows the supervised agent
    // reads), and on a fresh machine `data/` need not exist yet, so `requests::open`
    // creates it once. `tests/no_spawn_no_socket.rs` is the strict spec on that write
    // surface — it names the one read-write open, the one `INSERT`, and the archive's
    // `SQLITE_OPEN_READ_ONLY` — so this rule does not need to repeat it; it only needs
    // to know that `create_dir_all` in that file is not a second writer.
    const ALLOWED: &[(&str, &str, &str)] = &[
        (
            "atlas-desktop/src/requests.rs",
            "create_dir_all(",
            "the queue's parent directory, made once so the window has somewhere to write \
             its intent; the queue is the only write target (see tests/no_spawn_no_socket.rs)",
        ),
        (
            "atlas-desktop/src/requests.rs",
            "fs::set_permissions(",
            "the queue file's mode — 0600, before WAL is switched on so the `-wal` and `-shm` \
             inherit it, and again on the two side files an older install already made. It \
             writes no byte of any file: it answers who may read the reader's own words, which \
             SQLite creates 0644 under the usual umask. `tests/queue_payloads.rs` \
             (`the_queue_file_is_owner_only`) measures the mode, and `tests/disclosure.rs` \
             allows the same call in the same file for the same reason",
        ),
    ];
    let files = desktop_files();
    let mut problems = Vec::new();
    let mut used = vec![0usize; ALLOWED.len()];
    for needle in WRITERS {
        for (path, line, function, text) in find_wrapped(&files, needle, 3) {
            let mut excused = false;
            for (index, (file, allowed_needle, _)) in ALLOWED.iter().enumerate() {
                if *allowed_needle == *needle && path.ends_with(file) {
                    used[index] += 1;
                    excused = true;
                }
            }
            if excused {
                continue;
            }
            problems.push(format!("{path}:{line} ({function}): {text}"));
        }
    }
    for (index, count) in used.iter().enumerate() {
        assert!(
            *count > 0,
            "the exemption `{}` in {} matched nothing — the code moved, so delete the \
             exemption instead of leaving a rule that excuses nothing",
            ALLOWED[index].1,
            ALLOWED[index].0
        );
    }
    // …and the exemption is about the queue, so the file it excuses must still be the
    // queue: one read-write open of `data/requests.db`, no second database.
    let queue = files
        .iter()
        .find(|file| file.path.ends_with("atlas-desktop/src/requests.rs"))
        .expect("atlas-desktop/src/requests.rs is the window's queue");
    for needle in ["SQLITE_OPEN_READ_WRITE", "requests.db"] {
        assert!(
            queue.code.contains(needle),
            "{} no longer names `{needle}`, so the exemption above excuses the wrong file",
            queue.path
        );
    }
    assert!(
        problems.is_empty(),
        "the window writes a file — it draws one archive and owns no directory of its \
         own, so a writer here keeps state outside `atlas-config`'s `Paths` and outside \
         `atlas-platform`'s 0600/no-follow rules:\n{}\n\
         fix: read what you need (the archive answers it in a few queries), or put the \
         file under the app's own data dir through `atlas-platform::private` — that is a \
         product decision and a line in docs/THREAT-MODEL.md §5, not a test edit.",
        problems.join("\n")
    );
}

/// The writer rule has seen a hit, and the exemption it relies on is the scanner's.
///
/// A rule proved only on a tree that passes is a rule nobody has watched fire. The first
/// half runs the same `find_wrapped` over a synthetic shipped file. The second half
/// checks *why* the desktop's own archive test — which makes a temp directory and a
/// writable copy of the archive — is not a finding: `shipped_code` never handed those
/// files over. If that filter ever stops working, this test says so here instead of
/// turning the rule above into a false alarm that the next reader deletes.
#[test]
fn the_writer_rule_fires_on_shipped_code_and_the_test_filter_is_what_spares_a_test() {
    let attack = SourceFile {
        path: "atlas-desktop/src/views/graph.rs".to_string(),
        code: "fn keep_layout(bytes: &[u8]) {\n    std::fs::write(\"graph.layout\", bytes).ok();\n}\n"
            .to_string(),
    };
    let hits = find_wrapped(std::slice::from_ref(&attack), "fs::write(", 3);
    assert_eq!(
        hits.len(),
        1,
        "the rule the test above applies did not fire on a `fs::write` in shipped code, \
         so a green result there proves nothing: {hits:?}"
    );
    let shipped: Vec<String> = desktop_files().into_iter().map(|file| file.path).collect();
    for test_module in [
        "atlas-desktop/src/data/tests.rs",
        "atlas-desktop/src/tests.rs",
    ] {
        assert!(
            !shipped.iter().any(|path| path == test_module),
            "`{test_module}` is test code and the scan handed it to the writer rule — the \
             exemption the rule leans on is `atlas_guard::source::shipped_code`, and it is \
             not dropping `#[cfg(test)] mod tests;` modules any more"
        );
    }
    assert!(
        shipped.len() >= 10,
        "the scan read only {} shipped files under {} — a green writer rule would mean \
         nothing",
        shipped.len(),
        DESKTOP_SRC
    );
}

/// The default feature sets `atlas-desktop` still inherits, and from where.
///
/// A default feature is somebody else's list of what this binary needs — that is the
/// whole reason `no_dependency_is_declared_with_its_own_default_features` turns every
/// one of them off by hand. This list is empty: `egui` was the last pin that did not say
/// `default-features = false` (it inherited `default_fonts`, which eframe's own
/// `default_fonts` feature already turns on for this build), and that word has landed.
/// The test below still fails when a new pin starts inheriting defaults, which is what
/// keeps the list from being re-populated quietly.
const INHERITED_DEFAULTS: &[(&str, &str, &str)] = &[];

#[test]
fn atlas_desktop_is_a_member_with_pinned_dependencies() {
    // The rules at the top of this file read `[workspace.dependencies]`, and
    // `atlas-desktop` does not use that table: its four pins are declared inline in
    // `atlas-desktop/Cargo.toml`, where `every_dependency_is_pinned_to_an_exact_version`
    // and `no_dependency_is_declared_with_its_own_default_features` cannot see them. A
    // member whose pins live outside the audited table is a genuinely unexamined
    // surface — a range, a `path =` dependency or a decompressor could arrive there
    // without a single test noticing. So the same rules are applied to the same kind
    // of table, in the crate that draws the window.
    let manifest = workspace_manifest();
    let member_names = members(&manifest);
    assert!(
        member_names.iter().any(|member| member == "atlas-desktop"),
        "atlas-desktop is not a workspace member: `cargo build -p atlas-desktop` from \
         `rust/` builds it, so the workspace has to name it (`members`, next to the \
         comment that says what it is)"
    );
    let dir = rust_dir().join("atlas-desktop");
    assert!(
        dir.join("src").join("main.rs").is_file(),
        "atlas-desktop has no `src/main.rs`: this audit is about the binary a reader \
         double-clicks, and a library target would make the whole file a claim about \
         something else"
    );
    let text = read(&dir.join("Cargo.toml"));
    assert!(
        text.contains("publish = false"),
        "atlas-desktop is publishable: a crate that is not published cannot be a name \
         somebody else installs, and every member of this workspace says `publish = false`"
    );
    let entries = desktop_dependencies(&text);
    assert!(
        entries.len() >= 4,
        "the parser read only {} dependencies out of atlas-desktop/Cargo.toml",
        entries.len()
    );
    let mut problems: Vec<String> = Vec::new();
    for (name, value) in &entries {
        match declared_version(value) {
            // `=1.2.3` is exact. Anything else is a range cargo may walk without
            // anyone editing this file, i.e. an SBOM change and a silent rebuild.
            Some(version) if version.starts_with('=') => {}
            Some(version) => problems.push(format!("{name}: version = \"{version}\" is a range")),
            None => problems.push(format!("{name}: no version at all ({value})")),
        }
        for forbidden in ["path =", "git =", "registry =", "branch =", "rev =", "tag ="] {
            if value.contains(forbidden) {
                problems.push(format!(
                    "{name} names `{forbidden}`: a dependency comes from crates.io at a \
                     pinned version with a checksum, or it does not come at all"
                ));
            }
        }
        if value.contains("features =") && !value.contains("default-features = false") {
            problems.push(format!(
                "{name} picks features but keeps its defaults: `default-features = false` \
                 is what makes the feature list a decision ({value})"
            ));
        }
        let recorded = INHERITED_DEFAULTS
            .iter()
            .any(|(dependency, _, _)| dependency == name);
        if !value.contains("default-features = false") && !recorded {
            problems.push(format!(
                "{name} keeps its default features: either say \
                 `default-features = false` or record the set it inherits in \
                 INHERITED_DEFAULTS with a sentence saying why ({value})"
            ));
        }
    }
    for (name, features, why) in INHERITED_DEFAULTS {
        match entries.iter().find(|(dependency, _)| dependency == name) {
            None => problems.push(format!(
                "{name} is no longer a dependency of atlas-desktop: delete its \
                 INHERITED_DEFAULTS entry ({features})"
            )),
            Some((_, value)) if value.contains("default-features = false") => {
                problems.push(format!(
                    "{name} now turns its defaults off: delete its INHERITED_DEFAULTS \
                     entry ({why})"
                ))
            }
            Some(_) => {}
        }
    }
    // The lock is what a `--locked` build compiles, and it must name exactly the
    // dependencies the manifest declares. An extra name in the lock block is a
    // dependency that arrives from a table this file does not read (a
    // `[target.'cfg(…)'.dependencies]`), i.e. one nobody audited.
    let declared: std::collections::BTreeSet<String> =
        entries.iter().map(|(name, _)| name.clone()).collect();
    let packages = lock_package_edges(&lock_file());
    match packages.iter().find(|(name, _)| name == "atlas-desktop") {
        None => problems.push(
            "Cargo.lock has no `atlas-desktop` package: run `cargo build --offline` once \
             and commit the lock, or `--locked` builds will refuse this tree"
                .to_string(),
        ),
        Some((_, locked)) => {
            let locked: std::collections::BTreeSet<String> = locked.iter().cloned().collect();
            if locked != declared {
                problems.push(format!(
                    "atlas-desktop declares {declared:?} but Cargo.lock says {locked:?}: \
                     every dependency the binary links has to come from the one \
                     `[dependencies]` table this file audits — move it there, where the \
                     rules above can see it"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "atlas-desktop is not pinned like every other crate:\n{}",
        problems.join("\n")
    );
}
