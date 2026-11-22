//! Freshness: the workspace read as a *set of manifests*, and what a maintainer
//! would have to notice about it.
//!
//! `audit_manifest.rs` asks what the dependency graph may do — exact pins, no
//! build scripts, no decompressor below a body, one release profile. This file asks
//! the neighbouring question, the one a checkout goes stale in: is what the tree
//! *says about itself* still true, and has the world moved past it?
//!
//! Five claims, then a report that never fails.
//!
//! 1. **The workspace is all Rust.** Every `members` entry has a `src/` directory
//!    with `.rs` files in it, and nothing in a member's tree is another language's
//!    source. This is the product's oldest promise and the one a bulk copy breaks
//!    silently: a `.js` file, a `.py` helper, a `.c` shim dropped beside a crate
//!    does not fail any build — it just exists, and the next reader has to decide
//!    whether it is shipped. The scan is a *source* scan: `Cargo.toml`, `.md`, a
//!    fixture `.json` and a `.lock` are not a language, and data files are how a
//!    crate in this tree carries a fixture.
//!
//! 2. **Every dependency is an exact pin.** `=1.2.3`, never `1.2` or `^1.2.3`.
//!    `audit_manifest.rs` holds this for `[workspace.dependencies]`; a member may
//!    also declare a dependency inline (`atlas-desktop` pins its four there), and
//!    the same rule has to hold in every dependency table a member has —
//!    `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` and a
//!    `[target.'cfg(…)'.dependencies]`. A `path =` entry is a sibling crate in this
//!    workspace (nothing external, nothing to pin) and `workspace = true` inherits
//!    the pinned table — those are the only two shapes that may go without a
//!    version. And a name pinned twice must be pinned to the *same* version: one
//!    table cannot say `=1.0.154` while a member says `=1.0.155`.
//!
//! 3. **The MSRV is one number and it is not above the toolchain's.** Two ways to
//!    drift: a crate declaring a `rust-version` newer than the compiler that builds
//!    this tree (a build that cannot run, found late), or two crates declaring
//!    different ones (a repology question nobody answers). So: every declared
//!    `rust-version` is at or below the toolchain's, all declared values are equal,
//!    and a declared value is at least what its `edition` requires — an MSRV below
//!    the edition's floor is a number that cannot be true.
//!
//!    The toolchain is measured by running `rustc --version` (honouring `$RUSTC`).
//!    That is the *test* running the compiler it is already being compiled by, not
//!    the product: nothing in `src/` spawns anything (the guard's own audits scan
//!    `src/`, and this crate's binary only reads stdin). When no compiler can be
//!    asked — a sandbox with a bare checkout — the rule still fails on drift
//!    between crates, and says out loud that the ceiling was not measured.
//!
//! 4. **The lock names the versions the manifests pin** — on a handful of named
//!    crates, not the whole lock. `--locked` is the enforcement; this is the early
//!    warning, and it is worth naming the six that decide the product's character
//!    (the JSON writer, SQLite, the one HTTP client, the server, and the window's
//!    two toolkit pins) because a silent re-resolve of one of those is a different
//!    product. The pins are *read from the manifests*, never typed here twice, so
//!    the assertion cannot drift away from the files it is about.
//!
//! 5. **The window and the installer link nothing that fetches a web page.** The
//!    app's face is `atlas-desktop` and there is no network in it at all; the
//!    installer is `atlas-setup`, whose one transfer is an install step a human
//!    starts (`audit_downloads.rs` records it at the source level). What this file
//!    adds is the dependency half: no HTTP client in either closure, except the
//!    installer's, and only while it is written down below with the reason — and no
//!    *launcher* in either (a crate that hands a stored URL to the OS is the other
//!    way a database row becomes a request off this machine).
//!
//! # The report (`cargo test --offline -p atlas-guard --test audit_freshness -- --nocapture`)
//!
//! For every pinned dependency, the newest version the *local* registry index cache
//! knows — the same bytes `cargo update` would read, already on disk, so the report
//! needs no network and no `cargo` subprocess. It never fails: a pin that has moved
//! is not a defect, and a test that fails on one is a test that gets `#[ignore]`d.
//! What it does assert is that the report saw the pins at all — a report that
//! silently read nothing is worse than no report.
//!
//! Measured when this file was written (`cargo tree --offline --locked` and `ls` on
//! this machine): 25 members, 561 `.rs` files, no other source file anywhere in a
//! member, one declared `rust-version` (`atlas-desktop`, `1.85`, toolchain `1.100.0`),
//! the window's closure carrying no HTTP client and no launcher, and the installer's
//! carrying `ureq` — its `url` crate parses an address without resolving one, which is
//! why it is not on the fetcher list.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// the files these rules are about
// ---------------------------------------------------------------------------

fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn workspace_manifest() -> String {
    read(&rust_dir().join("Cargo.toml"))
}

fn lock_file() -> String {
    read(&rust_dir().join("Cargo.lock"))
}

/// The members the workspace builds, from `members = [ … ]`.
///
/// A member line is `"atlas-lib",` or a comment; the name is what sits between the
/// first pair of quotes. Hand-parsed because this crate has no dependencies and a
/// test that needed a TOML parser to prove the workspace is one language would be a
/// joke at its own expense.
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

/// Every dependency table of one manifest, as `(header, [(name, value)])`.
///
/// Values are kept as written, with a multi-line entry joined onto one line: the
/// `web-sys` feature list in `rust/Cargo.toml` spans thirty lines, and the shape of
/// its closing bracket is not something a rule should depend on. `[dependencies]`,
/// `[dev-dependencies]`, `[build-dependencies]` and any `[…]dependencies]` header a
/// target adds are all read — a dependency hidden in a target table is still a
/// dependency the build links.
fn dependency_tables(manifest: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut header = String::new();
    let mut open: Option<(String, String, i64)> = None;
    let depth = |text: &str| {
        let opens = text.matches('{').count() + text.matches('[').count();
        let closes = text.matches('}').count() + text.matches(']').count();
        opens as i64 - closes as i64
    };
    let flush = |header: &str, open: &mut Option<(String, String, i64)>, out: &mut Vec<(String, Vec<(String, String)>)>| {
        if let Some((name, value, _)) = open.take() {
            match out.iter_mut().find(|(seen, _)| seen == header) {
                Some((_, entries)) => entries.push((name, value)),
                None => out.push((header.to_string(), vec![(name, value)])),
            }
        }
    };
    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some((_, value, remaining)) = open.as_mut() {
            if !trimmed.is_empty() {
                value.push(' ');
                value.push_str(trimmed);
            }
            *remaining += depth(trimmed);
            if *remaining <= 0 {
                flush(&header, &mut open, &mut out);
            }
            continue;
        }
        if trimmed.starts_with('[') {
            header = trimmed.to_string();
            continue;
        }
        if !header.ends_with("dependencies]") && !header.ends_with("dependencies]".trim()) {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq) = trimmed.find('=') else { continue };
        let name = trimmed[..eq].trim().to_string();
        let value = trimmed[eq + 1..].trim().to_string();
        let remaining = depth(&value);
        if value.starts_with('{') && remaining > 0 {
            open = Some((name, value, remaining));
        } else {
            flush(&header, &mut open, &mut out);
            match out.iter_mut().find(|(seen, _)| *seen == header) {
                Some((_, entries)) => entries.push((name, value)),
                None => out.push((header.clone(), vec![(name, value)])),
            }
        }
    }
    flush(&header, &mut open, &mut out);
    out
}

/// The version a dependency value declares, as written: `"=1.2.3"` or
/// `{ version = "=1.2.3", … }` both answer `Some("=1.2.3")`.
fn declared_version(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split('"').next();
    }
    let at = value.find("version")?;
    let rest = &value[at + "version".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    rest.strip_prefix('"')?.split('"').next()
}

/// `Some("1.2.3")` when the value pins exactly three numeric components with `=`.
fn exact_pin(value: &str) -> Option<&str> {
    let version = declared_version(value)?;
    let bare = version.strip_prefix('=')?;
    let mut parts = bare.split('.');
    let numeric = |part: &str| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit());
    let three = (0..3).all(|_| parts.next().map(numeric).unwrap_or(false));
    if three && parts.next().is_none() {
        Some(bare)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Cargo.lock, as edges rather than as a list
// ---------------------------------------------------------------------------

/// Every `[[package]]` of the lock, as `(name, version)`.
fn locked(lock: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut version = String::new();
    let mut in_block = false;
    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            if !name.is_empty() {
                out.push((std::mem::take(&mut name), std::mem::take(&mut version)));
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
        }
    }
    if !name.is_empty() {
        out.push((name, version));
    }
    out
}

/// One `[[package]]` block's dependency names. An entry may carry the version the
/// resolve chose (`"bitflags 2.13.2",`), so the name is the first token.
fn lock_edges(lock: &str) -> Vec<(String, Vec<String>)> {
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
                let token = piece.trim().trim_matches('"').split_whitespace().next().unwrap_or("");
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

/// Every crate the named member is compiled against, by name — the transitive
/// closure of its `dependencies` edges in the lock. By name and not by
/// `(name, version)` because the lock is a graph and the rules here ask "can this
/// binary link that crate at all".
fn closure(lock: &str, root: &str) -> BTreeSet<String> {
    let mut edges: BTreeMap<String, Vec<String>> = Default::default();
    for (name, deps) in lock_edges(lock) {
        edges.entry(name).or_default().extend(deps);
    }
    let mut seen: BTreeSet<String> = Default::default();
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

// ---------------------------------------------------------------------------
// versions and the toolchain
// ---------------------------------------------------------------------------

/// `1.2.3` as `(1, 2, 3)`. A pre-release (`1.0.0-beta.1`) keeps its numeric head and
/// is marked by the caller, because "newest" for a pin means the newest *release*.
fn version_key(text: &str) -> Option<(u64, u64, u64, bool)> {
    let release = text.split(['-', '+']).next().unwrap_or(text);
    let pre = release.len() != text.len() && text.contains('-');
    let mut parts = release.split('.');
    let mut next = || parts.next().and_then(|part| part.parse::<u64>().ok());
    Some((next()?, next()?, next().unwrap_or(0), pre))
}

/// The newest released version in a list, ignoring pre-releases.
fn newest(versions: &[String]) -> Option<&str> {
    let mut best: Option<((u64, u64, u64), &str)> = None;
    for version in versions {
        let Some((major, minor, patch, pre)) = version_key(version) else { continue };
        if pre {
            continue;
        }
        let key = (major, minor, patch);
        if best.map(|(seen, _)| key > seen).unwrap_or(true) {
            best = Some((key, version.as_str()));
        }
    }
    best.map(|(_, version)| version)
}

/// The compiler this test is running under, from `$RUSTC` or `rustc` on `$PATH`.
///
/// A test may ask the compiler a question; the *product* may not spawn anything, and
/// every audit of that claim reads `src/`, where this does not live. The answer is
/// what makes "at or below the toolchain's" a measurement instead of a number
/// somebody typed: `rustc 1.100.0 (…)` parses to `(1, 100, 0)`.
fn toolchain_version() -> Option<(u64, u64, u64)> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = std::process::Command::new(rustc).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.split_whitespace().nth(1)?;
    let (major, minor, patch, _) = version_key(version)?;
    Some((major, minor, patch))
}

/// The MSRV floor each edition requires: an edition cannot build on a compiler older
/// than the release that introduced it, so a `rust-version` below this is a number
/// that cannot be true.
fn edition_floor(edition: &str) -> Option<(u64, u64, u64)> {
    match edition {
        "2015" => Some((1, 0, 0)),
        "2018" => Some((1, 31, 0)),
        "2021" => Some((1, 56, 0)),
        "2024" => Some((1, 85, 0)),
        _ => None,
    }
}

/// One package's `edition` and `rust-version`, as its own manifest declares them.
fn package_facts(manifest: &str) -> (Option<String>, Option<String>) {
    let mut edition = None;
    let mut rust_version = None;
    let mut in_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("edition = ") {
            edition = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = trimmed.strip_prefix("rust-version = ") {
            rust_version = Some(rest.trim().trim_matches('"').to_string());
        }
    }
    (edition, rust_version)
}

// ---------------------------------------------------------------------------
// the local registry index cache — the report's only input
// ---------------------------------------------------------------------------

/// `$CARGO_HOME/registry/index/index.crates.io-*`, the sparse index cache this
/// machine already holds. `None` on a vendored build or a bare checkout, which is a
/// reason for the report to say so, never a failure.
fn registry_index() -> Option<PathBuf> {
    let cargo_home = match std::env::var("CARGO_HOME") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".cargo"),
    };
    let index = cargo_home.join("registry").join("index");
    for entry in std::fs::read_dir(index).ok()? {
        let path = entry.ok()?.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        if path.is_dir() && name.starts_with("index.crates.io-") {
            return Some(path);
        }
    }
    None
}

/// Where cargo caches one crate's index file: `1/x`, `2/xy`, `3/x/xyz`, or
/// `xy/zw/xyzw…` — the first one, two or three characters of the *lowercased* name.
fn cache_entry(index: &Path, name: &str) -> Option<PathBuf> {
    let lower = name.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let relative = match chars.len() {
        0 => return None,
        1 => format!("1/{lower}"),
        2 => format!("2/{lower}"),
        3 => format!("3/{}/{lower}", chars[0]),
        _ => format!(
            "{}{}/{}{}/{lower}",
            chars[0], chars[1], chars[2], chars[3]
        ),
    };
    let path = index.join(".cache").join(relative);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// Every version a cached index file names, published or not.
///
/// The file is the sparse index's cache: a short binary header, then records
/// separated by NUL bytes, each record a `{"name":…,"vers":…,"yanked":…}` object
/// preceded by its etag. Only `"vers"` is read — the version list is what the report
/// needs, and a field-name search cannot be thrown off by a dependency's `"req"`.
fn cached_versions(path: &Path) -> Vec<String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for record in text.split('\0') {
        let Some(at) = record.find("\"vers\":\"") else { continue };
        if !record.trim_start().starts_with('{') {
            continue;
        }
        let rest = &record[at + "\"vers\":\"".len()..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// (a) the workspace is all Rust
// ---------------------------------------------------------------------------

/// Another language's *source* file: a program a reader would wonder about, not a
/// fixture. Data (`.json`, `.geojson`, `.txt`), prose (`.md`), a manifest
/// (`.toml`) and a lock are how this tree carries its own fixtures, and none of
/// them is a language the product could ship a second implementation in.
const OTHER_LANGUAGES: &[&str] = &[
    "py", "pyw", "pyc", "pyo", "js", "mjs", "cjs", "jsx", "ts", "tsx", "go", "rb", "java", "kt",
    "kts", "swift", "c", "h", "cc", "cpp", "cxx", "hpp", "cs", "php", "lua", "pl", "pm", "r", "jl",
    "dart", "scala", "hs", "ml", "ex", "exs", "erl", "clj", "zig", "nim", "vb", "f90", "asm", "s",
];

/// Every file under `dir`, as paths relative to it. `target/` is skipped: it is
/// generated, and a build of this workspace writes megabytes a scan should not walk.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let entries = match std::fs::read_dir(&next) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "target" && name != ".git" {
                    stack.push(path);
                }
                continue;
            }
            out.push(path);
        }
    }
    out
}

#[test]
fn every_member_is_rust() {
    let manifest = workspace_manifest();
    let members = members(&manifest);
    // 25 members until this task removed `atlas-web`, `atlas-ui`, `atlas-browser`
    // and `atlas-e2e` from the workspace; 21 is the list the parser must read back.
    assert_eq!(
        members.len(),
        21,
        "the member parser read {} members out of `members = [ … ]` — the list or the \
         cursor is wrong, and a green result below would mean nothing",
        members.len()
    );
    let mut problems: Vec<String> = Vec::new();
    let mut rust_files = 0usize;
    let mut seen_files = 0usize;
    for member in &members {
        let dir = rust_dir().join(member);
        if !dir.is_dir() {
            problems.push(format!("{member}: a member with no directory"));
            continue;
        }
        let src = dir.join("src");
        if !src.is_dir() {
            problems.push(format!(
                "{member}: no `src/` — a member of this workspace is a Rust crate, and \
                 cargo cannot build one without it"
            ));
            continue;
        }
        let mut in_src = 0usize;
        for path in walk(&src) {
            seen_files += 1;
            if path.extension().map(|ext| ext == "rs").unwrap_or(false) {
                in_src += 1;
                rust_files += 1;
            }
        }
        if in_src == 0 {
            problems.push(format!(
                "{member}/src: not one `.rs` file — this workspace is Rust, and a member \
                 whose sources are another language is not a member"
            ));
        }
        for path in walk(&dir) {
            let extension = path
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if OTHER_LANGUAGES.contains(&extension.as_str()) {
                let shown = path.strip_prefix(&rust_dir()).unwrap_or(&path);
                problems.push(format!(
                    "{}: a `.{extension}` source file inside a member",
                    shown.display()
                ));
            }
        }
    }
    // A walk that read nothing proves nothing. It counted 561 `.rs` files before this
    // task deleted four members (`atlas-web`, `atlas-ui`, `atlas-browser`,
    // `atlas-e2e`); the 21 left hold 376, so the floor is 350 — low enough for a
    // member to lose a file, high enough to catch a walk that stopped early.
    assert!(
        seen_files > 350,
        "the walk read only {seen_files} files under the members' `src/` (expected hundreds)"
    );
    assert!(
        rust_files > 350,
        "the walk counted only {rust_files} `.rs` files (it counted 561 before the web face left)"
    );
    assert!(
        problems.is_empty(),
        "this workspace is not all Rust:\n{}\n\
         fix: a member's sources are `.rs`. Another language's source inside a member is \
         either a leftover of the migration or a second implementation to maintain and \
         audit — delete it, or port it. Fixtures (`.json`, `.geojson`, `.txt`) and prose \
         (`.md`) are not sources and are fine.",
        problems.join("\n")
    );
}

// ---------------------------------------------------------------------------
// (b) every dependency is an exact pin
// ---------------------------------------------------------------------------

#[test]
fn every_dependency_is_an_exact_pin() {
    let manifest = workspace_manifest();
    // The workspace table first: it is the one `audit_manifest.rs` also reads, so a
    // failure here means the two rules disagree about the same file.
    let tables = dependency_tables(&manifest);
    let workspace_table = tables
        .iter()
        .find(|(header, _)| header == "[workspace.dependencies]")
        .map(|(_, entries)| entries.clone())
        .expect("`[workspace.dependencies]` is the pinned table");
    // Six workspace pins since this task dropped the seven that only the web face
    // used (axum, tower, futures-core, tungstenite, wasm-bindgen*, js-sys, web-sys):
    // serde, serde_json, rusqlite, ureq, url, tokio. A floor, not a count, because a
    // seventh pin is a decision, not a parser bug.
    assert!(
        workspace_table.len() >= 6,
        "the parser read only {} dependencies out of [workspace.dependencies]",
        workspace_table.len()
    );
    let mut problems: Vec<String> = Vec::new();
    // Every external pin, wherever it is declared, so a name pinned twice can be
    // compared with itself.
    let mut pins: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, value) in &workspace_table {
        match exact_pin(value) {
            Some(version) => {
                pins.entry(name.clone()).or_default().insert(version.to_string());
            }
            None => problems.push(format!(
                "[workspace.dependencies] {name}: {} is not an `=x.y.z` pin",
                declared_version(value).unwrap_or(value)
            )),
        }
    }
    for member in members(&manifest) {
        let text = read(&rust_dir().join(&member).join("Cargo.toml"));
        for (header, entries) in dependency_tables(&text) {
            for (name, value) in entries {
                let sibling = value.contains("path =") || value.starts_with("{ path");
                let inherited = value.contains("workspace = true");
                if inherited {
                    assert!(
                        workspace_table.iter().any(|(pinned, _)| *pinned == name),
                        "{member}/{header}: `{name}` inherits `workspace = true`, and \
                         `[workspace.dependencies]` does not pin it — cargo would refuse \
                         this manifest, and the pin it means to inherit does not exist"
                    );
                    continue;
                }
                if sibling && declared_version(&value).is_none() {
                    // A sibling crate: nothing external, nothing to pin, nothing to
                    // fetch. Its own manifest is audited as a member of its own.
                    continue;
                }
                match exact_pin(&value) {
                    Some(version) => {
                        pins.entry(name.clone()).or_default().insert(version.to_string());
                    }
                    None => problems.push(format!(
                        "{member}/{header} {name}: {} is not an `=x.y.z` pin",
                        declared_version(&value).unwrap_or(&value)
                    )),
                }
            }
        }
    }
    for (name, versions) in &pins {
        if versions.len() > 1 {
            problems.push(format!(
                "{name} is pinned to {}: one table says one version and another says \
                 another, so the lock cannot satisfy both pins and `--locked` builds \
                 will disagree about which bytes are compiled",
                versions.iter().cloned().collect::<Vec<_>>().join(" and ")
            ));
        }
    }
    assert!(
        pins.len() >= 9,   // 15 before the web-face pins left with their crates
        "the scan found only {} distinct pinned dependencies — it is not reading the \
         tables it claims to read",
        pins.len()
    );
    assert!(
        problems.is_empty(),
        "a dependency is not pinned exactly:\n{}\n\
         fix: every external dependency says `version = \"=x.y.z\"`. A caret range is a \
         silent SBOM change — `cargo update` inside it swaps the bytes that decide what \
         a URL may open and how a page is decoded, while the manifest still looks pinned. \
         A sibling crate (`path = …`) needs no version; `workspace = true` inherits the \
         pinned table.",
        problems.join("\n")
    );
}

// ---------------------------------------------------------------------------
// (c) the MSRV is one number, and the toolchain is above it
// ---------------------------------------------------------------------------

#[test]
fn the_msrv_is_one_number_and_the_toolchain_is_not_below_it() {
    let manifest = workspace_manifest();
    let toolchain = toolchain_version();
    let mut declared: Vec<(String, String)> = Vec::new();
    let mut undeclared: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut editions: BTreeSet<String> = BTreeSet::new();
    for member in members(&manifest) {
        let text = read(&rust_dir().join(&member).join("Cargo.toml"));
        let (edition, rust_version) = package_facts(&text);
        let edition = edition.unwrap_or_else(|| {
            problems.push(format!("{member}: no `edition` in [package] — a crate without one is edition 2015 by default, which nobody chooses on purpose"));
            "2015".to_string()
        });
        editions.insert(edition.clone());
        let Some(floor) = edition_floor(&edition) else {
            problems.push(format!("{member}: `edition = \"{edition}\"` is not an edition this workspace knows (2015, 2018, 2021, 2024)"));
            continue;
        };
        match rust_version {
            Some(version) => {
                let Some((major, minor, patch, pre)) = version_key(&version) else {
                    problems.push(format!("{member}: `rust-version = \"{version}\"` is not a version"));
                    continue;
                };
                // cargo accepts `1.85` as well as `1.85.0`, and both name one release.
                let dots = version.matches('.').count();
                if pre || !(dots == 1 || dots == 2) {
                    problems.push(format!(
                        "{member}: `rust-version = \"{version}\"` is not a `major.minor[.patch]` \
                         release"
                    ));
                    continue;
                }
                let key = (major, minor, patch);
                if key < floor {
                    problems.push(format!(
                        "{member}: `rust-version = \"{version}\"` is below what `edition = \
                         \"{edition}\"` requires ({}.{}.{}) — the number cannot be true",
                        floor.0, floor.1, floor.2
                    ));
                }
                if let Some(toolchain) = toolchain {
                    if key > toolchain {
                        problems.push(format!(
                            "{member}: `rust-version = \"{version}\"` is above this \
                             toolchain ({}.{}.{}): the tree cannot build the compiler it \
                             says it needs",
                            toolchain.0, toolchain.1, toolchain.2
                        ));
                    }
                }
                declared.push((member.clone(), version));
            }
            None => undeclared.push(member.clone()),
        }
    }
    // Drift between crates: one declared MSRV, or none. Two crates answering
    // differently is the shape of a rule nobody owns.
    let distinct: BTreeSet<&String> = declared.iter().map(|(_, version)| version).collect();
    if distinct.len() > 1 {
        problems.push(format!(
            "the members declare {} different `rust-version`s ({}): one of them is the \
             product's floor and the others are opinions",
            distinct.len(),
            declared
                .iter()
                .map(|(member, version)| format!("{member} = {version}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    match toolchain {
        Some((major, minor, patch)) => println!(
            "MEASURED toolchain: {major}.{minor}.{patch}  ·  declared MSRV: {}  ·  {} of {} \
             members declare none  ·  editions in use: {}",
            distinct
                .iter()
                .map(|version| version.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            undeclared.len(),
            declared.len() + undeclared.len(),
            editions.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        None => println!(
            "MEASURED toolchain: none — no `rustc --version` answered, so the ceiling on a \
             declared `rust-version` was not checked this run (drift between crates still was)"
        ),
    }
    assert!(
        !declared.is_empty() || !undeclared.is_empty(),
        "no member's manifest was read at all"
    );
    assert!(
        problems.is_empty(),
        "the MSRV is not one number at or below the toolchain's:\n{}\n\
         fix: `rust-version` is the oldest compiler this tree builds with — one value, \
         declared the same wherever it is declared, not older than the edition requires \
         (2024 needs 1.85) and never newer than the toolchain that builds the release. \
         A crate that declares none inherits the workspace's floor by omission; that is \
         allowed, and the printed report says which ones do.",
        problems.join("\n")
    );
}

// ---------------------------------------------------------------------------
// (d) the lock names the versions the manifests pin
// ---------------------------------------------------------------------------

/// The handful this rule asserts on, and why these six: each one decides the
/// product's character rather than a convenience. `serde_json` writes the archive's
/// JSON, `rusqlite` is the SQLite binding the whole store sits on, `ureq` is the one
/// HTTP client, `axum` is the server the wasm page (and the API) run on, and
/// `eframe`/`egui` are the window. A silent re-resolve of one of these is a
/// different product, and the pin is read from the manifest — never typed here.
// `axum` was here until the web face was deleted (this task): it is no longer pinned
// anywhere, so keeping it would make this rule ask about a crate the tree does not
// declare. The rest are the pins that decide the bytes: the store, the one HTTP
// client, and the window's two.
const NAMED_PINS: &[&str] = &["serde_json", "rusqlite", "ureq", "eframe", "egui"];

#[test]
fn the_lock_names_the_versions_the_manifests_pin() {
    let manifest = workspace_manifest();
    let lock = lock_file();
    let packages = locked(&lock);
    assert!(
        packages.len() > 100,
        "the lock parser read only {} packages",
        packages.len()
    );
    // Where each pin is declared: the workspace table, or the member that pins it
    // inline. The *version* comes from those files, so the assertion cannot drift —
    // and *every* declaration is kept, because two manifests pinning one crate to two
    // versions is itself the bug this test is looking for (a lock can only satisfy
    // one of them, and `--locked` will refuse the tree).
    let mut pinned_at: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    let mut record = |name: &str, version: &str, source: String| {
        pinned_at
            .entry(name.to_string())
            .or_default()
            .entry(version.to_string())
            .or_default()
            .push(source);
    };
    for (name, value) in dependency_tables(&manifest)
        .into_iter()
        .find(|(header, _)| header == "[workspace.dependencies]")
        .map(|(_, entries)| entries)
        .expect("[workspace.dependencies]")
    {
        if let Some(version) = exact_pin(&value) {
            record(&name, version, "rust/Cargo.toml".to_string());
        }
    }
    for member in members(&manifest) {
        let text = read(&rust_dir().join(&member).join("Cargo.toml"));
        for (header, entries) in dependency_tables(&text) {
            for (name, value) in entries {
                if let Some(version) = exact_pin(&value) {
                    record(&name, version, format!("{member}/{header}"));
                }
            }
        }
    }
    assert!(
        pinned_at.len() >= 9,   // the same nine: the workspace table plus the window's inline pins
        "the scan found only {} pinned dependencies — it is not reading the tables it          claims to read",
        pinned_at.len()
    );
    let mut problems: Vec<String> = Vec::new();
    for named in NAMED_PINS {
        let Some(versions) = pinned_at.get(*named) else {
            problems.push(format!("`{named}` is not pinned anywhere — the manifests changed shape and this rule is now asking about nothing"));
            continue;
        };
        if versions.len() > 1 {
            problems.push(format!(
                "`{named}` is pinned to {} ({}): the lock can name one of them, so \
                 \"the version the manifest pins\" is not one thing",
                versions.keys().cloned().collect::<Vec<_>>().join(" and "),
                versions
                    .iter()
                    .map(|(version, sources)| format!("={version} at {}", sources.join(", ")))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            continue;
        }
        let (pinned, sources) = versions.iter().next().expect("one pin");
        let source = sources.join(", ");
        let found: Vec<&String> = packages
            .iter()
            .filter(|(name, _)| name == named)
            .map(|(_, version)| version)
            .collect();
        if found.is_empty() {
            problems.push(format!(
                "Cargo.lock does not name `{named}`, which {source} pins to {pinned}: run \
                 `cargo build --offline` once and commit the lock"
            ));
            continue;
        }
        if found.len() > 1 {
            problems.push(format!(
                "Cargo.lock resolves `{named}` to {} versions ({}): the pin in {source} \
                 cannot hold for both, and `--locked` will refuse this tree",
                found.len(),
                found.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
            ));
            continue;
        }
        if found[0] != pinned {
            problems.push(format!(
                "`{named}`: {source} pins ={pinned}, Cargo.lock says {}. The bytes compiled \
                 would not be the bytes the manifest names",
                found[0]
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the lock and the manifests disagree:\n{}\n\
         fix: `cargo build --offline` rewrites the lock from the pins, then commit it — \
         and if the pin itself moved, say so in the report instead of editing the lock.",
        problems.join("\n")
    );
}

// ---------------------------------------------------------------------------
// (e) the window and the installer link no web-page fetcher
// ---------------------------------------------------------------------------

/// The crates that *fetch*: a client that dials, or a server that answers. Not a
/// content list — each name is a socket, and `url` is deliberately absent because it
/// parses an address without ever resolving one or opening anything (measured: it is
/// in both closures below and it does no I/O).
const WEB_FETCHERS: &[&str] = &[
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

/// The one crate that is allowed to fetch, the member that links it, and why.
///
/// `atlas-setup` is not the app: no crate *links* it, so no binary the product runs
/// contains its client. It is *spawned*, once, by the boot: `atlas-launcher/src/setup.rs`
/// runs `atlas-setup --install` when the model file or `bin/llama-server` is missing, and
/// `--no-install` forbids that (`audit_downloads.rs` records the spawn site, its argv and
/// its reason — an earlier wording here said `atlas start` never spawns it, which stopped
/// being true when the launcher gained the setup step, and a test comment that denies a
/// call site is how a reviewer is talked out of reading it). Its single transfer is the
/// model file, from an install step a human started (`audit_downloads.rs` records the call
/// site and the gate). A record here is not a blessing — it fails the moment the entry
/// stops being true, and a *second* fetcher anywhere fails the moment it appears.
const INSTALL_FETCHERS: &[(&str, &str, &str)] = &[(
    "atlas-setup",
    "ureq",
    "the model download, from an install the boot starts when the model file is missing (--no-install forbids that) or one a human runs by hand: https, gated by atlas_guard::urlsafe on every hop, verified by size and sha256 before it is renamed into place (audit_downloads.rs)",
)];

/// Crates that hand a URL to the operating system instead of fetching it themselves.
///
/// A launcher is not a fetcher and is denied anyway: `webbrowser::open` builds an
/// `NSWorkspace` / `xdg-open` / `ShellExecute` request straight from the string, so a
/// row in the archive that reached `OpenUrl` could ask the system to start
/// `file:///…` or a scheme some other program registered. Measured when this rule was
/// written (`cargo tree --offline --locked -p atlas-desktop`): `webbrowser` is **not**
/// in this lock at all — eframe 0.37 puts it behind `egui-winit`'s `webbrowser`
/// feature, which this workspace's `default-features = false` never turns on — so
/// neither crate links one, and the day one appears the rule fires rather than
/// recording it.
const URL_LAUNCHERS: &[&str] = &["webbrowser", "opener", "open", "xdg", "open-sauce"];

#[test]
fn the_window_and_the_installer_link_no_web_page_fetcher() {
    let lock = lock_file();
    let all = locked(&lock);
    let mut problems: Vec<String> = Vec::new();
    for (member, minimum) in [("atlas-desktop", 80usize), ("atlas-setup", 20usize)] {
        let crates = closure(&lock, member);
        // A walk that found nothing proves nothing: the window's closure is over a
        // hundred crates, the installer's over twenty (measured with `cargo tree`).
        assert!(
            crates.len() > minimum,
            "{member}: the closure walk found only {} crates — a green result below would \
             mean nothing",
            crates.len()
        );
        assert!(
            all.iter().any(|(name, _)| name == member),
            "{member} is not in Cargo.lock: the walk started somewhere else"
        );
        for fetcher in WEB_FETCHERS {
            if !crates.contains(*fetcher) {
                continue;
            }
            let recorded = INSTALL_FETCHERS
                .iter()
                .any(|(owner, name, _)| *owner == member && *name == *fetcher);
            if !recorded {
                problems.push(format!(
                    "{member} links `{fetcher}`: a web client in the {} closure",
                    if member == "atlas-desktop" {
                        "window's"
                    } else {
                        "installer's unrecorded"
                    }
                ));
            }
        }
        for launcher in URL_LAUNCHERS {
            if crates.contains(*launcher) {
                problems.push(format!(
                    "{member} links `{launcher}`, which hands a URL to the operating \
                     system: a stored address becomes a request to launch whatever scheme \
                     it names, and the reader sees only the label"
                ));
            }
        }
    }
    // The records are descriptions of real edges; a stale one hides the next one.
    for (member, fetcher, _) in INSTALL_FETCHERS {
        if !closure(&lock, member).contains(*fetcher) {
            problems.push(format!(
                "the record says {member} links `{fetcher}`, and it no longer does: delete \
                 the entry (or say what replaced it)"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "a crate that must not fetch is linked to something that can:\n{}\n\
         fix: the window is a reader — `atlas-desktop` links no HTTP client, no server and \
         no tunnelling transport (its one socket is `src/http.rs`, a POST to the local \
         model on a 127.0.0.1 literal). The installer's one fetch is an install step, and \
         a new fetcher there is a new decision: write it down in INSTALL_FETCHERS with the \
         gate it goes through, or drop it.",
        problems.join("\n")
    );
}

// ---------------------------------------------------------------------------
// the report: what has moved, without the network
// ---------------------------------------------------------------------------

#[test]
fn the_report_of_what_has_moved() {
    // Printed, never asserted on: a pin the world has moved past is not a defect, and
    // a test that fails on one becomes a test somebody marks `#[ignore]`. What *is*
    // asserted is that the report read the pins — a report of nothing is worse than no
    // report, because it looks like an answer.
    let manifest = workspace_manifest();
    let mut pins: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (name, value) in dependency_tables(&manifest)
        .into_iter()
        .find(|(header, _)| header == "[workspace.dependencies]")
        .map(|(_, entries)| entries)
        .expect("[workspace.dependencies]")
    {
        if let Some(version) = exact_pin(&value) {
            pins.insert(name, (version.to_string(), "rust/Cargo.toml".to_string()));
        }
    }
    for member in members(&manifest) {
        let text = read(&rust_dir().join(&member).join("Cargo.toml"));
        for (header, entries) in dependency_tables(&text) {
            for (name, value) in entries {
                if let Some(version) = exact_pin(&value) {
                    pins.entry(name)
                        .or_insert((version.to_string(), format!("{member}/{header}")));
                }
            }
        }
    }
    assert!(
        pins.len() >= 9,   // the report looks up the same nine
        "the report found only {} pinned dependencies to look up",
        pins.len()
    );

    let index = registry_index();
    let mut moved: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    println!(
        "\nPINS: {} across {} manifests · index cache: {}",
        pins.len(),
        members(&manifest).len() + 1,
        match &index {
            Some(path) => path.display().to_string(),
            None => "none found (vendored checkout?) — versions not compared".to_string(),
        }
    );
    for (name, (pinned, source)) in &pins {
        let newest = index
            .as_ref()
            .and_then(|index| cache_entry(index, name))
            .map(|path| cached_versions(&path))
            .and_then(|versions| newest(&versions).map(|newest| newest.to_string()));
        match newest {
            Some(newest) => {
                let pinned_key = version_key(pinned).map(|(a, b, c, _)| (a, b, c));
                let newest_key = version_key(&newest).map(|(a, b, c, _)| (a, b, c));
                let behind = match (pinned_key, newest_key) {
                    (Some(pinned_key), Some(newest_key)) => pinned_key < newest_key,
                    _ => false,
                };
                if behind {
                    moved.push(format!("{name} {pinned} → {newest}"));
                    println!("  moved   {name:<28} pinned ={pinned:<12} newest {newest:<12} ({source})");
                } else {
                    println!("  current {name:<28} pinned ={pinned:<12} newest {newest:<12} ({source})");
                }
            }
            None => {
                unknown.push(name.clone());
                println!(
                    "  unknown {name:<28} pinned ={pinned:<12} not in the index cache ({source})"
                );
            }
        }
    }
    println!(
        "\nMEASURED: {} pinned dependencies · {} have moved in the index cache · {} are not \
         cached here · {} as expected\n",
        pins.len(),
        moved.len(),
        unknown.len(),
        pins.len() - moved.len() - unknown.len()
    );
}
