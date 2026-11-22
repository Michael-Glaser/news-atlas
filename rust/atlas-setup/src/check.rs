//! What a fresh machine has, and what it is missing — ten rows, one per piece.
//!
//! This is the first thing a person runs on a machine that has never seen ATLAS, so the
//! wording is the feature: every row names the file it means, says whether it is there,
//! and — when it is not — says in plain words what to do and where the file comes from.
//! A row either offers an action this machine can take with no download (`atlas-setup
//! llama --yes`, or the `cargo build` that writes the window — spelled for the platform
//! the report prints on, because `rust/build.sh` is a `bash` script and a fresh Windows
//! install has no `bash`), or names the exact path of a file the person has to supply,
//! plus where that file lives.
//!
//! A row is a measurement, never an assumption: the window is a file that exists, the
//! archive and the outlines are files it opens, the settings are the number the window
//! reads, the weights are a byte count compared with `models/model.json`.
//!
//! Nothing here starts what it measures. The window is checked as a file and never
//! launched; the port row is a statement of what the program does, not a probe, because
//! probing the port a window does not open would open it.
//!
//! The ten rows are the window's needs, not the retired web app's. The Rust toolchain,
//! the nine engine binaries and the vendored llama.cpp *source* are not rows: the window
//! runs without any of them, and a `cargo build --release` in `rust/` is what a person
//! runs to get them.

use std::path::Path;

use crate::{llama, model, Layout, Line};

/// The most a settings file may be, in bytes — the window's own cap, so the report and
/// the window never disagree about which files count.
const MAX_SETTINGS: u64 = 64 * 1024;

/// The port the window falls back to when `data/config.json` names none. The window's
/// documented default (`atlas_desktop::app`), repeated here so the report can print the
/// number instead of saying "some default".
const DEFAULT_PORT: u16 = 8080;

/// Every piece, in the order a person meets them on a fresh machine.
///
/// `verify_digest` reads all 6.5 GB of weights and compares the digest, not just the
/// size; `check` does not do that by default, because a first run that takes a minute
/// stops being run.
pub fn run(layout: &Layout, verify_digest: bool) -> Vec<Line> {
    let root = &layout.root;
    let settings = settings(root);
    vec![
        window(root),
        page_view(),
        archive(root),
        map_outlines(root),
        config(settings.clone()),
        model_file(root, verify_digest),
        model_served(root),
        llama_binary(root),
        port(settings.port()),
        start_command(root),
    ]
}

/// Is every piece there?
pub fn all_ok(lines: &[Line]) -> bool {
    lines.iter().all(|line| line.ok)
}

// --- the rows ---------------------------------------------------------------------

/// The program itself: `rust/target/release/atlas-desktop`.
///
/// Presence, never a launch: starting it would open a GPU surface on a machine that may
/// have no session, and would answer a different question than the one this row asks.
fn window(root: &Path) -> Line {
    let path = root.join("rust").join("target").join("release").join(exe("atlas-desktop"));
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {
            Line::ok("window", format!("{} ({})", path.display(), size_text(meta.len())))
        }
        _ => Line::missing(
            "window",
            "rust/target/release/atlas-desktop is not built".to_string(),
            build_hint(),
        ),
    }
}

/// The command that writes the window binary, spelled for the machine reading it.
///
/// `bash rust/build.sh` is a POSIX sentence: a fresh Windows install has no `bash`, and
/// there is no `build.ps1` beside the script to point at instead. What `build.sh` *does*
/// is one `cargo build --release --offline` inside `rust/`, so that is what the Windows
/// row offers — a command that exists on all three platforms, and the path spelled the
/// way that platform spells it. A hint a reader cannot run is worse than no hint: it
/// sends them to a command-not-found after they have already been told something is
/// missing.
fn build_hint() -> String {
    build_hint_for(cargo_present(), cfg!(windows))
}

/// [`build_hint`] without the detection, so both answers can be pinned by a test.
///
/// `has_cargo == false` is the state of a machine that has never built anything: it has
/// no `cargo`, so "build it" would send the person to a command-not-found. Rust is
/// installed by hand, from one address, and that is the sentence they need first.
fn build_hint_for(has_cargo: bool, windows: bool) -> String {
    if !has_cargo {
        return if windows {
            "install Rust once from https://rustup.rs (by hand — nothing here downloads \
             it), open a new terminal, then build it: cargo build --release --offline, run \
             in rust\\ — it writes rust\\target\\release\\atlas-desktop.exe"
                .to_string()
        } else {
            "install Rust once from https://rustup.rs (by hand — nothing here downloads \
             it), open a new terminal, then build it: bash rust/build.sh — it writes \
             rust/target/release/atlas-desktop"
                .to_string()
        };
    }
    if windows {
        "build it: cargo build --release --offline, run in rust\\ — it writes \
         rust\\target\\release\\atlas-desktop.exe, compiled from this repository"
            .to_string()
    } else {
        "build it: bash rust/build.sh — it writes rust/target/release/atlas-desktop, \
         compiled from this repository"
            .to_string()
    }
}

/// Is there a `cargo` on `PATH`?
///
/// A directory listing, not a spawn: `check` reads, and a report that starts a process
/// to decide what to print is a report that can hang. The name is the one the platform
/// actually installs (`cargo.exe` on Windows).
fn cargo_present() -> bool {
    let name = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    match std::env::var_os("PATH") {
        Some(path) => std::env::split_paths(&path).any(|dir| dir.join(name).is_file()),
        None => false,
    }
}

/// What the page view draws with, on this platform.
///
/// The window has one view that renders a page the app did not draw: its reader. On macOS
/// that is WebKit, which ships with the system; on Windows it is the Microsoft Edge
/// WebView2 runtime, a separate install that may be absent; on Linux this build does not
/// compile the embedder at all, so the reader shows the article as text.
///
/// This row is deliberately *not* a requirement row. A machine without WebView2 still runs
/// the window and still opens articles — as text — so a missing runtime is a warning, and
/// `check` still exits 0. Nothing here starts a process, opens a socket, downloads or
/// installs anything: it reports what is on the machine, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageView {
    /// macOS: the system's own web view.
    WebKit,
    /// Windows: the separately installed runtime, and whether it is here.
    WebView2(bool),
    /// Everything else (this build: Linux): the text reader.
    Text,
}

/// The page view this build has, by platform.
fn page_view() -> Line {
    page_view_for(platform_page_view())
}

/// [`page_view`] without the detection, so all three sentences can be pinned by tests on
/// whichever machine runs them.
fn page_view_for(view: PageView) -> Line {
    match view {
        PageView::WebKit => Line::ok(
            "page view",
            "on macOS the page view uses WebKit, which is part of the system - there is \
             nothing to install, and ATLAS still starts no browser of its own",
        ),
        PageView::WebView2(true) => Line::ok(
            "page view",
            "on Windows the page view uses the Microsoft Edge WebView2 runtime, and it is \
             installed on this machine - there is nothing to add",
        ),
        // A warning, not a refusal: the text reader answers.
        PageView::WebView2(false) => Line::warn(
            "page view",
            "on Windows the page view would use the Microsoft Edge WebView2 runtime, but it is \
             not installed here, so the text reader answers and articles open as text; install \
             the runtime from Microsoft by hand (nothing here downloads it) and the next start \
             uses it",
        ),
        PageView::Text => Line::ok(
            "page view",
            "on Linux the page view is the text reader - the rendered page view would need \
             WebKitGTK, which this build does not use, so articles open as text",
        ),
    }
}

/// Which of the three this build is.
fn platform_page_view() -> PageView {
    if cfg!(target_os = "macos") {
        PageView::WebKit
    } else if cfg!(windows) {
        PageView::WebView2(webview2_present())
    } else {
        PageView::Text
    }
}

/// Is the WebView2 runtime installed? Directories only: no spawn, no registry call, no
/// download.
///
/// The runtime lives in one of three places — the per-machine folder, the per-user folder
/// for an install that did not need an administrator, and inside Microsoft Edge itself,
/// which registers its own copy as the runtime. Each of those holds one directory per
/// version, and a version directory holds `msedgewebview2.exe`. Finding that program is the
/// cheapest check that is still about the program the loader looks for; a folder that
/// exists but is empty is not an installed runtime.
fn webview2_present() -> bool {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for variable in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        let Some(base) = std::env::var_os(variable) else { continue };
        let base = std::path::PathBuf::from(base);
        for product in ["EdgeWebView", "Edge"] {
            roots.push(base.join("Microsoft").join(product).join("Application"));
        }
    }
    roots.iter().any(|dir| {
        let Ok(entries) = std::fs::read_dir(dir) else { return false };
        entries
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.path().join("msedgewebview2.exe").is_file())
    })
}

/// The article database: `data/atlas.db`, which the window only ever reads.
fn archive(root: &Path) -> Line {
    let path = root.join("data").join("atlas.db");
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {
            Line::ok("archive", format!("{} ({})", path.display(), size_text(meta.len())))
        }
        _ => Line::missing(
            "archive",
            "there is no archive yet, so the window opens with no articles".to_string(),
            "you supply this one: data/atlas.db is the database ATLAS collects articles into — \
             it is not in this repository and it is never downloaded",
        ),
    }
}

/// The country outlines the map draws: `public/vendor/ne_50m_countries.geojson`.
///
/// The same three names, in the same order, as `atlas_desktop::assets::geojson`, so a row
/// reports the file the window would actually open.
fn map_outlines(root: &Path) -> Line {
    let dir = root.join("public").join("vendor");
    for name in ["ne_50m_countries.geojson", "ne_50m_countries.geo.json", "countries.geo.json"] {
        let path = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.is_file() {
                return Line::ok(
                    "map outlines",
                    format!("{} ({})", path.display(), size_text(meta.len())),
                );
            }
        }
    }
    Line::missing(
        "map outlines",
        "the map has no country outlines to draw".to_string(),
        "restore public/vendor/ne_50m_countries.geojson from this repository — it ships \
         with ATLAS, and it is never downloaded",
    )
}

/// The settings the window reads: `data/config.json`, and the one number in it.
///
/// Missing is not a failure — the window has a documented default and says nothing — so
/// the row is `ok` with the default printed. A file that is there but unusable *is* a
/// failure, because it silently changes which port chat dials.
fn config(settings: Settings) -> Line {
    match settings {
        Settings::Port(port) => Line::ok("config", format!("data/config.json: the model server is on port {port}")),
        Settings::Absent => Line::ok(
            "config",
            format!("no data/config.json — the window uses the default: the model server on port {DEFAULT_PORT}"),
        ),
        Settings::NoPort => Line::ok(
            "config",
            format!("data/config.json names no llama_port — the window uses the default: the model server on port {DEFAULT_PORT}"),
        ),
        Settings::Unusable(reason) => Line::missing(
            "config",
            format!("data/config.json is there but {reason}"),
            "write {\"llama_port\": 8080} into data/config.json, or delete the file to use the default",
        ),
    }
}

/// What the window would dial: no port of its own, and the model server's if you run one.
///
/// A statement, not a probe. The window opens no listening socket at all; the only port
/// it knows is the model server's, because chat has to reach it. Checking that claim by
/// connecting would open the very socket the row says does not exist, so the row prints
/// the number the settings name and the fact that nothing is listening on ATLAS's behalf.
fn port(port: u16) -> Line {
    Line::ok(
        "port",
        format!(
            "no port is opened by the window; chat dials port {port} on this machine only, and \
             only if you start the model server. Nothing listens while the weights are \
             downloaded either: that is one outbound connection a person starts"
        ),
    )
}

/// The weights: `models/<name>.gguf`, at the size `models/model.json` pins.
fn model_file(root: &Path, verify_digest: bool) -> Line {
    // `pin`, not `load_manifest`: the repository ships no weights and therefore no
    // `models/model.json` either, so a fresh clone has nothing to read — and the pin the
    // install uses is the one built into `atlas-setup`. A manifest that *is* there
    // overrides it, and a broken one is still a sentence naming the file.
    let plan = match model::pin(root) {
        Ok(plan) => plan,
        Err(error) => {
            return Line::missing(
                "model file",
                format!("the list of expected files cannot be read: {error}"),
                "restore models/model.json from this repository — it names the weights file, \
                 where it comes from, its size and its fingerprint",
            )
        }
    };
    let path = root.join("models").join(&plan.name);
    let shown = format!("models/{}", plan.name);
    let size = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => {
            return Line::missing(
                "model file",
                if plan.pins_size() {
                    format!("{shown} is not there ({} of weights)", size_text(plan.bytes))
                } else {
                    format!("{shown} is not there yet — the app downloads it once it is asked to")
                },
                format!(
                    "you supply this one: download it once from {} and keep it at {shown} — \
                     it is not in this repository. This is the one download in ATLAS, started \
                     by hand with atlas-setup --install (or atlas-setup model --yes); the app \
                     itself never downloads anything at run time",
                    plan.url
                ),
            )
        }
    };
    if plan.pins_size() && size != plan.bytes {
        return Line::missing(
            "model file",
            format!("{shown} is {size} bytes; this copy of ATLAS expects {}", plan.bytes),
            format!(
                "replace {shown} with the file models/model.json names, or run \
                 atlas-setup model --yes --fetch"
            ),
        );
    }
    if !verify_digest {
        return Line::ok(
            "model file",
            if plan.pins_size() {
                format!("{shown} ({}, the size this copy of ATLAS expects)", size_text(plan.bytes))
            } else {
                format!("{shown} ({})", size_text(size))
            },
        );
    }
    match model::verify_file(&path) {
        Ok((bytes, digest)) if !plan.pins_digest() => Line::ok(
            "model file",
            format!(
                "{shown} ({bytes} bytes; contents hashed, and no fingerprint is pinned — \
                 {digest} is what this copy will load)"
            ),
        ),
        Ok((bytes, digest)) if digest == plan.sha256 => {
            Line::ok("model file", format!("{shown} ({bytes} bytes, contents verified)"))
        }
        Ok(_) => Line::missing(
            "model file",
            format!("{shown} is not the file this copy of ATLAS expects: its contents differ"),
            format!(
                "replace {shown} with the file models/model.json names (its fingerprint starts \
                 {})",
                plan.sha256.chars().take(12).collect::<String>()
            ),
        ),
        Err(error) => Line::missing(
            "model file",
            error,
            format!("make {shown} readable, or replace it with the file models/model.json names"),
        ),
    }
}

/// Which model file is *in force*: what `data/config.json` names, or the pinned one.
///
/// The row above says whether the pinned file is here. This row answers the other
/// question a reader actually has — which file the app will load — and it answers it from
/// the settings the CLI writes: `model_local`, read through `atlas-config` (the same
/// validation and the same path policy `atlas-setup --import-model` uses), never through a
/// second parser here.
///
/// Four states, all of them spoken, and no silent fallback between them:
///
/// * the settings file cannot be used — a failure that says so and points at the row above;
/// * nothing named — the file the installer pinned is in force, named with its size;
/// * named, and the path policy refused it — a failure, because a refused setting is not a
///   default;
/// * named and here — that file, its size, and the fact that the settings chose it;
///   named and not here — a failure, with the two ways out.
fn model_served(root: &Path) -> Line {
    let paths = atlas_config::Paths::at(root);
    let config = match atlas_config::Config::load_at(&paths) {
        Ok(config) => config,
        Err(_) => {
            return Line::missing(
                "model served",
                "data/config.json cannot be used, so which model ATLAS loads is not known - the \
                 config row above says what is wrong with the file",
                "fix data/config.json, or delete it: with no file at all, the file the installer \
                 pinned is used",
            )
        }
    };
    // What the *file* says, not what the effective settings say: `atlas-config` fills an
    // empty `model_local` with the pinned path (that is its documented default), so only the
    // stored object can tell "nothing named" from "named and refused".
    let stored_named = config
        .stored
        .get("model_local")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let effective = config.model_local.trim().to_string();

    if stored_named.is_empty() {
        let (name, known) = match model::pin(root) {
            Ok(plan) => {
                let pinned_bytes = plan.pins_size().then_some(plan.bytes);
                let here = std::fs::metadata(root.join("models").join(&plan.name))
                    .ok()
                    .filter(|meta| meta.is_file())
                    .map(|meta| meta.len());
                (plan.name, here.or(pinned_bytes))
            }
            Err(_) => (String::new(), None),
        };
        let what = if name.is_empty() {
            "the file the installer pinned".to_string()
        } else {
            match known {
                Some(bytes) => format!("models/{name} ({})", size_text(bytes)),
                None => format!("models/{name} (not there yet)"),
            }
        };
        return Line::ok(
            "model served",
            format!(
                "data/config.json names no model_local, so the file the installer pinned is in \
                 force: {what}"
            ),
        );
    }

    if effective.is_empty() {
        // Stored, and the settings policy refused it: not a default, a setting nothing will
        // honour.
        return Line::missing(
            "model served",
            format!(
                "data/config.json names {stored_named}, which the settings policy refuses, so no \
                 file is chosen"
            ),
            "name a file inside this ATLAS folder: run atlas-setup --import-model \
             /absolute/path/model.gguf to copy the one you have into models/, or remove the \
             model_local line to load the pinned file again",
        );
    }

    let path = Path::new(&effective);
    let shown = path.display().to_string();
    match std::fs::metadata(path).ok().filter(|meta| meta.is_file()) {
        Some(meta) => Line::ok(
            "model served",
            format!(
                "{shown} ({}) — the app loads this one: data/config.json names it in model_local",
                size_text(meta.len())
            ),
        ),
        None => Line::missing(
            "model served",
            format!(
                "{shown} is not there, and data/config.json names it in model_local, so the app \
                 has nothing to load"
            ),
            "put the file at that path, or run atlas-setup --import-model \
             /absolute/path/model.gguf to copy the one you already have (it is checked for the \
             GGUF signature first), or remove the model_local line to load the pinned file again",
        ),
    }
}

/// The model server program: `bin/llama-server`, built from the copy of llama.cpp that
/// ships in `tools/llama.cpp`.
fn llama_binary(root: &Path) -> Line {
    if llama::server_present(root) {
        let path = layout_of(root).model_server();
        let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        return Line::ok("llama.cpp binary", format!("{} ({})", path.display(), size_text(size)));
    }
    Line::missing(
        "llama.cpp binary",
        "the model server program is not built".to_string(),
        llama_build_hint(llama::cmake_available().ok()),
    )
}

/// What to do about a missing model server, given what `cmake --version` answered.
///
/// Separate from the row so both halves can be pinned: with cmake it is one build, and
/// without it the sentence is the one `llama.rs` prints — the same words in the report
/// and in the refusal, with the package managers named, because the person reading it is
/// on one of the three platforms this project runs on and does not want a link.
fn llama_build_hint(cmake: Option<String>) -> String {
    let build = "build it: atlas-setup llama --yes — it writes bin/llama-server from the copy \
                 of llama.cpp in tools/llama.cpp (nothing is downloaded; it takes a few minutes)";
    match cmake {
        // With cmake the only news is the version, inside the same sentence.
        Some(cmake) => format!("{build}; {cmake} is here"),
        // Without it the install sentence stands on its own line, in the words the
        // refusal uses, so the report cannot grow a second, staler instruction.
        None => format!("{build}. {}", llama::cmake_install_sentence()),
    }
}

/// The command that opens the window: `run.sh` here, `run.ps1` on Windows.
fn start_command(root: &Path) -> Line {
    let name = start_script_name();
    let path = root.join(name);
    let meta = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta,
        _ => {
            return Line::missing(
                "start command",
                format!("there is no {name} to start the window with"),
                format!("restore {name} from this repository — it ships with ATLAS and runs the window"),
            )
        }
    };
    if executable(&meta) {
        Line::ok("start command", format!("{} — it opens the window", path.display()))
    } else {
        Line::missing(
            "start command",
            format!("{name} is there but cannot be run"),
            format!("make it runnable: chmod +x {name}"),
        )
    }
}

// --- the settings file, read once ---------------------------------------------------

/// What `data/config.json` says, under the same rules the window applies.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Settings {
    /// No file at all: the window keeps its default and says nothing.
    Absent,
    /// A port the window can dial.
    Port(u16),
    /// A usable file that names no port: the window keeps its default.
    NoPort,
    /// A file that is there and cannot be used, with the reason in plain words.
    Unusable(&'static str),
}

impl Settings {
    /// The port the window would dial.
    fn port(&self) -> u16 {
        match self {
            Settings::Port(port) => *port,
            _ => DEFAULT_PORT,
        }
    }
}

/// Read `data/config.json` the way `atlas_desktop::assets::model_port` does: size cap,
/// JSON, one integer key in `1..=65535`, and never a guess.
fn settings(root: &Path) -> Settings {
    settings_at(&root.join("data").join("config.json"))
}

/// [`settings`] over a named file, so a test can hand it a fixture.
fn settings_at(path: &Path) -> Settings {
    let meta = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return Settings::Absent,
    };
    if meta.len() > MAX_SETTINGS {
        return Settings::Unusable("it is larger than 64 KB");
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Settings::Unusable("it cannot be read");
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        // Not an object (`[]`, `"x"`, a bare number): nothing in it is a setting, so this is
        // a broken file rather than one that names no port.
        return Settings::Unusable("it is not settings this program understands");
    };
    match map.get("llama_port") {
        Some(named) => match named.as_u64() {
            Some(port) if (1..=65535).contains(&port) => Settings::Port(port as u16),
            _ => Settings::Unusable("its llama_port is not a port number"),
        },
        // A settings file that names no port is not a broken one: the window falls back to
        // its documented default, and `data/config.json` legitimately holds nothing but
        // `model_local` after `atlas-setup --import-model`. Reporting that as a failure told
        // a reader their working settings were broken.
        None => Settings::NoPort,
    }
}

// --- small helpers ------------------------------------------------------------------

/// The start script's name on this platform.
fn start_script_name() -> &'static str {
    if cfg!(windows) { "run.ps1" } else { "run.sh" }
}

/// Can this file be run? On Windows a file is enough; elsewhere the execute bit decides,
/// because `./run.sh` without it is a permission error and a report that said "ok" would
/// be lying about the one command the row is about.
#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    true
}

fn layout_of(root: &Path) -> Layout {
    Layout { root: root.to_path_buf() }
}

/// A byte count as a person says it: `6.5 GB`, `512 MB`, `10 bytes`.
///
/// One function, because "0 GB" for a ten-byte file (and "0 GB" for a 900 MB one) is the
/// kind of number that makes a report unreadable.
fn size_text(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        // One decimal: "6 MB" for a 5.9 MB program is a round number that does not
        // describe the file in front of the reader.
        format!("{:.1} MB", value / MB)
    } else if value >= 1_000.0 {
        format!("{:.0} kB", value / 1_000.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// The binary's name on this platform.
fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch root, removed first so two runs of the suite cannot see each other.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-check-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rust")).expect("a scratch root");
        std::fs::write(dir.join("rust").join("Cargo.toml"), "[workspace]\n").expect("a crate root");
        dir
    }

    /// A manifest with a one-paragraph URL, so the model row has an origin to name.
    fn manifest(name: &str, bytes: usize) -> String {
        format!(
            r#"{{"name":"{name}","repo":"a/b","url":"https://example.invalid/{name}","bytes":{bytes},"sha256":"{}","license":"x"}}"#,
            "a".repeat(64)
        )
    }

    /// A machine that has never built anything has no `cargo`: the window row must name
    /// that first, and nothing in it may imply ATLAS downloads or installs Rust itself.
    #[test]
    fn the_window_row_names_the_missing_toolchain_before_the_build() {
        for windows in [false, true] {
            let without = build_hint_for(false, windows);
            assert!(
                without.contains("rustup.rs"),
                "the one address Rust comes from is missing: {without}"
            );
            assert!(
                without.contains("by hand"),
                "a person installs it, not ATLAS: {without}"
            );
            assert!(
                without.contains("nothing here downloads"),
                "the row must not suggest a download: {without}"
            );
            let with = build_hint_for(true, windows);
            assert!(
                !with.contains("rustup.rs"),
                "a machine that can build is not told to install Rust: {with}"
            );
            // Both spellings name the file the row is about, which is what makes the
            // instruction followable.
            let file = if windows { "atlas-desktop.exe" } else { "atlas-desktop" };
            assert!(without.contains(file), "{without}");
            assert!(with.contains(file), "{with}");
        }
    }

    /// The report and the refusal use one cmake sentence: two versions of "install cmake"
    /// is how a person follows the stale one.
    #[test]
    fn the_llama_row_says_where_cmake_comes_from() {
        let missing = llama_build_hint(None);
        assert!(missing.contains(llama::cmake_install_sentence()), "{missing}");
        for hint in ["brew install cmake", "apt install cmake", "winget install"] {
            assert!(missing.contains(hint), "no {hint} in {missing}");
        }
        assert!(missing.contains("atlas-setup llama --yes"), "{missing}");
        assert!(missing.contains("tools/llama.cpp"), "{missing}");

        // With cmake there is no install sentence: the version is the news.
        // One sentence, not a parenthesis inside a parenthesis: the install sentence is
        // whole, and the build sentence is closed before it starts.
        assert!(missing.contains("few minutes). cmake is not installed"), "{missing}");
        assert!(!missing.contains("minutes. It needs cmake:"), "{missing}");

        let present = llama_build_hint(Some("cmake 3.30.2".to_string()));
        assert!(present.contains("cmake 3.30.2"), "{present}");
        assert!(!present.contains("brew install"), "{present}");
    }

    /// A file that is present and small is not "0 MB": `bin/llama-server` on macOS is a
    /// 33 kB stub beside its dylibs, and "(0 MB)" for a working file reads as missing.
    #[test]
    fn a_small_present_file_is_reported_in_kilobytes() {
        assert_eq!(size_text(33_472), "33 kB");
        assert_eq!(size_text(999), "999 bytes");
        assert_eq!(size_text(1_460_000), "1.5 MB");
        assert_eq!(size_text(5_913_008), "5.9 MB");
        assert_eq!(size_text(6_526_302_432), "6.5 GB");
    }

    /// The report is exactly the ten rows a person meets, in that order.
    #[test]
    fn the_report_is_the_ten_rows_in_this_order() {
        let root = scratch("shape");
        let layout = Layout::at(&root).expect("a root");
        let lines = run(&layout, false);
        let pieces: Vec<&str> = lines.iter().map(|line| line.piece).collect();
        assert_eq!(
            pieces,
            vec![
                "window",
                "page view",
                "archive",
                "map outlines",
                "config",
                "model file",
                "model served",
                "llama.cpp binary",
                "port",
                "start command",
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fresh root fails every row that is about a file, and each one says what to do in
    /// words a person who has never seen this project can act on.
    #[test]
    fn an_empty_root_names_every_missing_piece_and_its_fix() {
        let root = scratch("empty");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("models").join("model.json"), manifest("m.gguf", 10))
            .expect("a manifest");
        let layout = Layout::at(&root).expect("a valid root");
        let lines = run(&layout, false);
        for line in &lines {
            if !line.ok {
                assert!(line.fix.is_some(), "{} says nothing to do: {line:?}", line.piece);
                assert!(line.text().contains("FAIL"), "{}", line.piece);
            }
        }
        // Six of the eight are about a file and are missing here; config is absent (not a
        // failure) and the port row is a statement.
        let failing: Vec<&str> = lines.iter().filter(|line| !line.ok).map(|line| line.piece).collect();
        assert_eq!(
            failing,
            vec!["window", "archive", "map outlines", "model file", "llama.cpp binary", "start command"],
            "exactly the file rows fail on an empty machine"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The page-view row says what this machine's page view is made of, and it names the
    /// platform in the same sentence a reader will repeat to somebody else.
    #[test]
    fn the_page_view_row_names_the_platform() {
        let mac = page_view_for(PageView::WebKit);
        assert!(mac.ok && !mac.warn, "{mac:?}");
        assert!(mac.detail.contains("macOS"), "{}", mac.detail);
        assert!(mac.detail.contains("WebKit"), "{}", mac.detail);
        assert!(mac.detail.contains("part of the system"), "{}", mac.detail);

        let windows = page_view_for(PageView::WebView2(true));
        assert!(windows.ok && !windows.warn, "{windows:?}");
        assert!(windows.detail.contains("Windows"), "{}", windows.detail);
        assert!(windows.detail.contains("WebView2"), "{}", windows.detail);

        let linux = page_view_for(PageView::Text);
        assert!(linux.ok && !linux.warn, "{linux:?}");
        assert!(linux.detail.contains("Linux"), "{}", linux.detail);
        assert!(linux.detail.contains("text reader"), "{}", linux.detail);
        assert!(linux.detail.contains("WebKitGTK"), "{}", linux.detail);

        // Every sentence is ASCII and says nothing that reads like a stack trace.
        for row in [mac, windows, linux] {
            assert!(row.detail.is_ascii(), "{}", row.detail);
            for jargon in ["gtk4", "msedge", "runtime error", "null"] {
                assert!(!row.detail.to_lowercase().contains(jargon), "{}", row.detail);
            }
        }
    }

    /// An absent runtime is a warning, not a refusal: the row says what happens instead,
    /// the line prints `warn` rather than `ok` or `FAIL`, and `all_ok` stays true so
    /// `check` exits 0 on a machine without it.
    #[test]
    fn an_absent_runtime_is_a_warning_and_not_a_refusal() {
        let absent = page_view_for(PageView::WebView2(false));
        assert!(absent.ok, "a missing runtime must not block the window: {absent:?}");
        assert!(absent.warn, "it is not silent either: {absent:?}");
        assert!(absent.fix.is_none(), "{absent:?}");
        assert!(absent.detail.contains("not installed"), "{}", absent.detail);
        assert!(absent.detail.contains("text reader answers"), "{}", absent.detail);
        assert!(absent.detail.contains("by hand"), "{}", absent.detail);
        assert!(absent.detail.contains("nothing here downloads it"), "{}", absent.detail);
        assert!(absent.detail.is_ascii(), "{}", absent.detail);

        assert!(absent.text().contains("warn"), "{}", absent.text());
        assert!(!absent.text().contains("FAIL"), "{}", absent.text());
        // And it does not take the exit code down with it.
        assert!(all_ok(&[absent.clone()]), "a warning is not a missing piece: {absent:?}");
        // The present runtime is an ordinary row: no warning about a piece that is here.
        let here = page_view_for(PageView::WebView2(true));
        assert!(here.ok && !here.warn, "{here:?}");
    }

    /// The row measures and reports; it writes nothing anywhere. Run for all three
    /// platforms, with the process's environment and working directory untouched, and the
    /// scratch root is byte-for-byte the file listing it was.
    #[test]
    fn the_page_view_row_writes_nothing() {
        let root = scratch("page-view-writes");
        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(root.join("data").join("note.txt"), b"a file that must not change")
            .expect("a file");

        let listing = |dir: &std::path::Path| -> Vec<(String, u64)> {
            let mut entries: Vec<(String, u64)> = std::fs::read_dir(dir)
                .expect("a readable directory")
                .filter_map(|entry| entry.ok())
                .map(|entry| {
                    let path = entry.path();
                    let len = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                    (path.file_name().unwrap_or_default().to_string_lossy().to_string(), len)
                })
                .collect();
            entries.sort();
            entries
        };
        let before = listing(&root);
        let before_data = listing(&root.join("data"));
        let before_root = std::env::current_dir().expect("a working directory");

        for view in [PageView::WebKit, PageView::WebView2(true), PageView::WebView2(false), PageView::Text] {
            let row = page_view_for(view);
            assert!(!row.detail.is_empty());
            assert!(row.fix.is_none(), "{row:?}");
        }
        // And the platform's own row, which is the one a person actually runs.
        let _ = page_view();

        assert_eq!(listing(&root), before, "the row created or changed a file");
        assert_eq!(listing(&root.join("data")), before_data, "data/ must be untouched");
        assert_eq!(
            std::env::current_dir().expect("a working directory"),
            before_root,
            "the row changed the working directory"
        );
        assert!(!root.join("data").join("setup.json").exists(), "no progress file from a report row");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The served row follows the settings file: two different `model_local` values are
    /// two different lines, each naming its own file and size, and neither of them claims
    /// the pinned file is in force.
    #[test]
    fn the_served_row_changes_when_model_local_changes() {
        let root = scratch("served-changes");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::create_dir_all(root.join("data")).expect("data");
        let small = root.join("models").join("small.gguf");
        let big = root.join("models").join("big.gguf");
        std::fs::write(&small, b"GGUF small").expect("a small file");
        std::fs::write(&big, vec![b'G'; 4_000]).expect("a bigger file");

        let row_for = |name: &str| {
            std::fs::write(
                root.join("data").join("config.json"),
                serde_json::json!({ "model_local": name }).to_string(),
            )
            .expect("settings");
            model_served(&root)
        };

        let small_row = row_for(small.to_str().expect("utf-8"));
        let big_row = row_for(big.to_str().expect("utf-8"));

        assert!(small_row.ok, "{small_row:?}");
        assert!(big_row.ok, "{big_row:?}");
        assert_ne!(small_row.detail, big_row.detail, "the row must change with the setting");
        assert!(small_row.detail.contains("small.gguf"), "{}", small_row.detail);
        assert!(small_row.detail.contains("10 bytes"), "{}", small_row.detail);
        assert!(big_row.detail.contains("big.gguf"), "{}", big_row.detail);
        assert!(big_row.detail.contains("4 kB"), "{}", big_row.detail);
        for row in [&small_row, &big_row] {
            assert!(row.detail.contains("model_local"), "{}", row.detail);
            assert!(row.detail.to_lowercase().contains("the app loads this one"), "{}", row.detail);
            assert!(
                !row.detail.contains("the file the installer pinned"),
                "a named file is not the pinned one: {}",
                row.detail
            );
            assert!(row.fix.is_none(), "{row:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `model_local` with nothing at it is a failure with a sentence — the path, the
    /// consequence, and the two ways out — not a quiet fall back to the pinned file. The
    /// app would start with no model at all, so `check` must not say "ok".
    #[test]
    fn a_missing_served_model_is_a_sentence_not_a_silent_fallback() {
        let root = scratch("served-missing");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::create_dir_all(root.join("data")).expect("data");
        let gone = root.join("models").join("gone.gguf");
        std::fs::write(
            root.join("data").join("config.json"),
            serde_json::json!({ "model_local": gone.to_str().expect("utf-8") }).to_string(),
        )
        .expect("settings");

        let row = model_served(&root);
        assert!(!row.ok, "{row:?}");
        assert!(row.detail.contains("gone.gguf"), "{}", row.detail);
        assert!(row.detail.contains("not there"), "{}", row.detail);
        assert!(row.detail.contains("nothing to load"), "{}", row.detail);
        assert!(
            !row.detail.contains("the file the installer pinned"),
            "a named file must not be described as the pinned one: {}",
            row.detail
        );
        let fix = row.fix.clone().unwrap_or_default();
        assert!(fix.contains("atlas-setup --import-model"), "{fix}");
        assert!(fix.contains("remove the model_local line"), "{fix}");
        // The whole line still reads as a line of this report.
        assert!(row.text().contains("FAIL"), "{}", row.text());
        assert!(row.text().contains("->"), "{}", row.text());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// With nothing named, the row says which file is in force and its size — the pinned
    /// one, by the name the pin gives it (never an empty "no model").
    #[test]
    fn with_no_model_local_the_pinned_file_is_in_force() {
        let root = scratch("served-pinned");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("models").join("model.json"), manifest("m.gguf", 10))
            .expect("a manifest");
        // No data/ at all: the state of a machine that has never run anything.
        let absent = model_served(&root);
        assert!(absent.ok, "{absent:?}");
        assert!(
            absent.detail.contains("the file the installer pinned is in force"),
            "{}",
            absent.detail
        );
        assert!(absent.detail.contains("models/m.gguf"), "{}", absent.detail);
        // The pin carries a size, so the row gives the size the installer will fetch.
        assert!(absent.detail.contains("models/m.gguf (10 bytes)"), "{}", absent.detail);

        // The pinned file present: the same sentence with the measured size.
        std::fs::write(root.join("models").join("m.gguf"), vec![b'x'; 4_000]).expect("weights");
        let here = model_served(&root);
        assert!(here.ok, "{here:?}");
        assert!(here.detail.contains("models/m.gguf (4 kB)"), "{}", here.detail);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A settings file that cannot be used is a sentence naming the file, and it does not
    /// pretend a model was chosen. This is the shape a hand-edited file takes.
    #[test]
    fn an_unusable_settings_file_names_the_file_and_chooses_nothing() {
        let root = scratch("served-bad");
        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(root.join("data").join("config.json"), b"[]").expect("a bad file");
        let row = model_served(&root);
        assert!(!row.ok, "{row:?}");
        assert!(row.detail.contains("data/config.json"), "{}", row.detail);
        let fix = row.fix.clone().unwrap_or_default();
        assert!(fix.contains("delete it"), "{fix}");
        assert!(fix.contains("the file the installer pinned is used"), "{fix}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file that names `model_local` and nothing else is a *good* settings file: since
    /// `--import-model` writes exactly that, the config row must not call it broken.
    #[test]
    fn a_settings_file_with_only_model_local_is_not_a_failure() {
        let root = scratch("served-only-model");
        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(
            root.join("data").join("config.json"),
            br#"{"model_local":"/tmp/somewhere/m.gguf"}"#,
        )
        .expect("settings");
        let row = config(settings(&root));
        assert!(row.ok, "{row:?}");
        assert!(row.detail.contains("names no llama_port"), "{}", row.detail);
        assert!(row.detail.contains("default"), "{}", row.detail);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every instruction names the exact path it is about, so "missing" is never a
    /// riddle. This is the wording the first run depends on.
    #[test]
    fn every_fix_names_the_exact_path_it_is_about() {
        let root = scratch("paths");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("models").join("model.json"), manifest("m.gguf", 10))
            .expect("a manifest");
        let layout = Layout::at(&root).expect("a root");
        let lines = run(&layout, false);

        let expected: [(&str, &str); 6] = [
            ("window", "rust/target/release/atlas-desktop"),
            ("archive", "data/atlas.db"),
            ("map outlines", "public/vendor/ne_50m_countries.geojson"),
            ("model file", "models/m.gguf"),
            ("llama.cpp binary", "bin/llama-server"),
            ("start command", start_script_name()),
        ];
        for (piece, path) in expected {
            let line = lines.iter().find(|line| line.piece == piece).expect(piece);
            let fix = line.fix.as_deref().unwrap_or("");
            assert!(fix.contains(path), "{piece} does not name {path}: {fix}");
        }

        // And where the file comes from, in plain words: a shipped file says so, a file
        // the person must supply is named as theirs and given a place to come from.
        let of = |piece: &str| {
            lines.iter().find(|line| line.piece == piece).and_then(|line| line.fix.clone()).unwrap_or_default()
        };
        assert!(of("map outlines").contains("from this repository"), "{}", of("map outlines"));
        assert!(of("start command").contains("from this repository"), "{}", of("start command"));
        assert!(of("archive").contains("you supply this one"), "{}", of("archive"));
        assert!(
            of("model file").contains("you supply this one") && of("model file").contains("https://example.invalid/m.gguf"),
            "the model row names its origin: {}",
            of("model file")
        );
        // The one file that is built rather than supplied offers the command that builds
        // it, and says it needs no download.
        assert!(of("llama.cpp binary").contains("atlas-setup llama --yes"), "{}", of("llama.cpp binary"));
        assert!(of("llama.cpp binary").contains("tools/llama.cpp"), "{}", of("llama.cpp binary"));
        assert!(
            of("window").contains(if cfg!(windows) { "atlas-desktop.exe" } else { "rust/build.sh" }),
            "{}",
            of("window")
        );
        // The sentence this test used to demand was `bash rust/build.sh` on every machine.
        // That is the wrong instruction on Windows, where `bash` is usually absent and the
        // binary is `atlas-desktop.exe`; `build_hint()` now answers for the platform it is
        // compiled for, so the assertion follows it instead of pinning the POSIX wording.
        assert!(
            cfg!(windows) == of("window").contains("atlas-desktop.exe"),
            "the window hint names this platform's binary: {}",
            of("window")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The wording test: a first run must not read like a stack trace. No wasm, no
    /// protocol names, no internal shorthands in either the detail or the fix.
    #[test]
    fn the_report_is_plain_english() {
        let root = scratch("wording");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("models").join("model.json"), manifest("m.gguf", 10))
            .expect("a manifest");
        // A config that is there and unusable, so the failure sentences are in the text.
        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(root.join("data").join("config.json"), b"[]").expect("a bad config");
        let layout = Layout::at(&root).expect("a root");
        // Paths and addresses are content — a row that names `data/config.json` or where
        // the weights come from is doing its job. The test is about the words *around*
        // them, so those tokens are dropped before the jargon is looked for.
        let prose: String = run(&layout, false)
            .iter()
            .flat_map(|line| {
                let text = format!("{} {}", line.detail, line.fix.as_deref().unwrap_or(""));
                text.split_whitespace()
                    .filter(|word| !word.contains('/') && !word.contains(':') && !word.contains('.'))
                    .map(|word| word.to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        for jargon in [
            "wasm",
            "loopback",
            "socket",
            "cdp",
            "egui",
            "http",
            "json",
            "stdin",
            "argv",
            "sha256",
            "binary", // said as "the program", not "the binary"
            "artifact",
            "manifest",
            "checksum",
            "daemon",
        ] {
            assert!(!prose.contains(jargon), "{jargon:?} is jargon in a first-run report:\n{prose}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The window is a file this check reads and never starts: a stand-in that would
    /// leave a sentinel behind if it were executed leaves nothing.
    #[test]
    fn the_window_is_checked_as_a_file_and_never_started() {
        let root = scratch("window");
        let dir = root.join("rust").join("target").join("release");
        std::fs::create_dir_all(&dir).expect("release");
        assert!(!window(&root).ok, "nothing is built yet");

        // A directory of the right name is not the program.
        std::fs::create_dir_all(dir.join(exe("atlas-desktop"))).expect("a directory");
        assert!(!window(&root).ok, "a directory is not the window");
        std::fs::remove_dir_all(dir.join(exe("atlas-desktop"))).expect("remove");

        let sentinel = root.join("started");
        let binary = dir.join(exe("atlas-desktop"));
        std::fs::write(&binary, format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()))
            .expect("a stand-in");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("an executable");
        }

        let line = window(&root);
        assert!(line.ok, "{line:?}");
        assert!(line.detail.contains("atlas-desktop"), "{line:?}");
        assert!(!sentinel.exists(), "check must not start the window");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The archive and the outlines are measured, and the outlines row follows the same
    /// three-name preference the window does.
    #[test]
    fn the_archive_and_the_outlines_are_measured() {
        let root = scratch("files");
        assert!(!archive(&root).ok, "no data/ yet");
        assert!(!map_outlines(&root).ok, "no public/vendor/ yet");

        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(root.join("data").join("atlas.db"), b"seven!!").expect("the archive");
        let line = archive(&root);
        assert!(line.ok, "{line:?}");
        assert!(line.detail.contains("7 bytes"), "the size is a measurement: {line:?}");

        let vendor = root.join("public").join("vendor");
        std::fs::create_dir_all(&vendor).expect("vendor");
        std::fs::write(vendor.join("countries.geo.json"), b"{}").expect("a fallback name");
        let line = map_outlines(&root);
        assert!(line.ok, "{line:?}");
        assert!(line.detail.contains("countries.geo.json"), "{line:?}");

        std::fs::write(vendor.join("ne_50m_countries.geojson"), b"{\"type\":\"FeatureCollection\"}")
            .expect("the preferred name");
        let line = map_outlines(&root);
        assert!(
            line.detail.contains("ne_50m_countries.geojson"),
            "the first name the window tries wins: {line:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
        assert!(!archive(&root).ok, "a removed root has no archive");
        assert!(!map_outlines(&root).ok, "a removed root has no outlines");
    }

    /// The settings row is the number the window reads, and a file that is there but
    /// unusable is a failure rather than a silent default.
    #[test]
    fn the_config_row_reads_the_model_server_port() {
        let root = scratch("config");
        let path = root.join("data").join("config.json");
        assert_eq!(settings(&root), Settings::Absent);
        let line = config(settings(&root));
        assert!(line.ok, "a missing file is not a failure: {line:?}");
        assert!(line.detail.contains(&DEFAULT_PORT.to_string()), "the default is printed: {line:?}");

        std::fs::create_dir_all(root.join("data")).expect("data");
        std::fs::write(&path, r#"{"app_port":8787,"llama_port":8081,"cdp_port":9222}"#)
            .expect("a settings file");
        assert_eq!(settings(&root), Settings::Port(8081));
        let line = config(settings(&root));
        assert!(line.ok, "{line:?}");
        assert!(line.detail.contains("8081"), "{line:?}");

        for bad in ["[]", r#"{"llama_port":"8081"}"#, r#"{"llama_port":0}"#, r#"{"llama_port":70000}"#] {
            std::fs::write(&path, bad).expect("a bad settings file");
            let line = config(settings(&root));
            assert!(!line.ok, "{bad} must not read as usable: {line:?}");
            assert!(line.detail.contains("data/config.json"), "the path is named: {line:?}");
            assert!(line.fix.as_deref().unwrap_or("").contains("data/config.json"), "{line:?}");
        }

        // A file that names no port is not a broken one: the window falls back to its
        // documented default, and `atlas-setup --import-model` writes exactly this shape
        // (`model_local` and nothing else). Reporting it as a failure made `check` exit 1
        // on a machine that runs the window fine.
        for quiet in ["{}", r#"{"model_local":"/x/y.gguf"}"#] {
            std::fs::write(&path, quiet).expect("a settings file with no port");
            let line = config(settings(&root));
            assert!(line.ok, "{quiet} names no port, which is the default: {line:?}");
            assert!(line.detail.contains(&DEFAULT_PORT.to_string()), "{line:?}");
            assert!(line.detail.contains("default"), "{line:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The port row states the design and prints the number chat would dial — it never
    /// probes, because the window opens no port for anything to probe.
    #[test]
    fn the_port_row_states_that_no_port_is_opened() {
        let line = port(8081);
        assert!(line.ok);
        assert!(line.detail.contains("no port is opened"), "{line:?}");
        assert!(line.detail.contains("8081"), "the number chat dials is printed: {line:?}");
        assert_eq!(Settings::Absent.port(), DEFAULT_PORT);
        assert_eq!(Settings::Unusable("x").port(), DEFAULT_PORT);
    }

    /// The start command is the script that opens the window, and a script that cannot be
    /// run is not a start command.
    #[test]
    fn the_start_command_row_checks_the_script_is_runnable() {
        let root = scratch("start");
        let path = root.join(start_script_name());
        assert!(!start_command(&root).ok, "nothing to start the window with");

        std::fs::write(&path, "#!/usr/bin/env bash\nexec rust/target/release/atlas-desktop\n")
            .expect("the script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(!start_command(&root).ok, "a script with no execute bit is not runnable");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("an executable");
        }
        let line = start_command(&root);
        assert!(line.ok, "{line:?}");
        assert!(line.detail.contains(start_script_name()), "{line:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file at the pinned size reads as present; one byte less does not. The size is the
    /// cheap check `check` makes on every run.
    #[test]
    fn the_model_line_compares_the_byte_count_with_the_expected_size() {
        let root = scratch("model");
        let body = b"0123456789";
        let mut manifest_text = manifest("m.gguf", body.len());
        manifest_text = manifest_text.replace(&"a".repeat(64), &crate::sha::sha256(body));
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("models").join("model.json"), manifest_text).expect("manifest");
        std::fs::write(root.join("models").join("m.gguf"), body).expect("the file");

        let line = model_file(&root, false);
        assert!(line.ok, "{line:?}");
        assert!(line.detail.contains("10 bytes"), "{line:?}");

        let verified = model_file(&root, true);
        assert!(verified.ok, "{verified:?}");
        assert!(verified.detail.contains("contents verified"), "{verified:?}");

        std::fs::write(root.join("models").join("m.gguf"), &body[..9]).expect("short file");
        let line = model_file(&root, false);
        assert!(!line.ok);
        assert!(line.detail.contains("9 bytes"), "{line:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A missing weights file names where it comes from — the pinned address — because no
    /// amount of looking on this machine will produce 6.5 GB of weights.
    #[test]
    fn the_missing_model_names_its_address() {
        let root = scratch("nopins");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("models").join("model.json"), manifest("m.gguf", 10))
            .expect("a manifest");
        let line = model_file(&root, false);
        assert!(!line.ok, "{line:?}");
        let fix = line.fix.as_deref().unwrap_or("");
        assert!(fix.contains("models/m.gguf"), "{fix}");
        assert!(fix.contains("https://example.invalid/m.gguf"), "{fix}");

        // No manifest at all is the *fresh clone*: `models/*` is ignored, so neither the
        // weights nor the manifest travel with the code. The row still names the exact
        // file and the exact address, because the pin is built into the crate.
        std::fs::remove_file(root.join("models").join("model.json")).expect("remove");
        let line = model_file(&root, false);
        assert!(!line.ok, "{line:?}");
        let fix = line.fix.as_deref().unwrap_or("");
        assert!(fix.contains("https://huggingface.co/"), "the built-in pin's address: {fix}");
        assert!(
            fix.contains("models/Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf"),
            "the built-in pin's file: {fix}"
        );
        assert!(fix.contains("atlas-setup --install"), "and the command that fetches it: {fix}");
        assert!(
            line.detail.contains("models/Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf"),
            "{}",
            line.detail
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
