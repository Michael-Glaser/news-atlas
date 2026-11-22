//! The shipped tree carries no web UI — and the eight scripts a person actually runs
//! never name one, build one, copy one, or open a page.
//!
//! # What this file is for
//!
//! The window is the face: `rust/target/release/atlas-desktop`, an egui window that reads
//! `data/atlas.db` read-only and opens no socket. It used to be a server plus a wasm page
//! (`public/ui/`, built by `rust/build-ui.sh` from `atlas-ui`), and a script that still
//! ran one of those steps after the page was retired would be worse than dead code: a
//! fresh machine would install a wasm toolchain, or copy a stale bundle, or — the case
//! that matters — start something that serves a page. So the rule is structural and it is
//! checked where a person enters: the launcher scripts at the root, and the one crate that
//! still fetches anything (`atlas-setup`).
//!
//! # Why these names are a list, and not a "word filter"
//!
//! Each entry is a *capability*, not copy: a build tool (`wasm-pack`, `trunk`), the wasm
//! target (`wasm32-…`), the crate and directory the retired page lived in (`atlas-ui`,
//! `public/ui`, `build-ui.sh`), and the calls that hand a URL to a program
//! (`xdg-open`, `Start-Process`, an `open <address>` form). A script that mentions none of
//! them cannot rebuild the page, cannot ship it, and cannot open it. Nothing here judges
//! what the scripts *say* — the English around them is free.
//!
//! # What is pinned besides the absence
//!
//! The two sentences a broken fresh machine depends on, so that removing the UI steps
//! cannot quietly take them with it: the build command the setup scripts run, and the
//! sentence that says what to do when a piece is missing (`--provision`, or the
//! subcommand the report named).
//!
//! # Proof that the scan bites
//!
//! [`the_scan_bites_when_a_ui_step_is_put_back`] runs the same two predicates over a
//! planted script that copies `public/ui/`, and over one that calls `xdg-open`; both must
//! produce a finding, so "no findings" cannot mean "the scanner is blind".

use std::path::{Path, PathBuf};

/// The scripts a person runs, exactly as they sit at the repository root.
///
/// Listed rather than discovered: a script that is renamed out of this list is a script
/// whose UI steps would stop being checked, and the first test below fails on the missing
/// file instead of passing on a smaller set.
const SCRIPTS: &[&str] = &[
    "ATLAS.command",
    "ATLAS.sh",
    "ATLAS.bat",
    "ATLAS.ps1",
    "setup.sh",
    "setup.ps1",
    "run.sh",
    "run.ps1",
];

/// A name that builds the retired page, ships it, or opens one. Every entry is a
/// capability: `wasm-pack` and `trunk` are the two builders, `wasm32-` is the target a
/// build needs, `atlas-ui`/`public/ui`/`build-ui.sh` are the crate, the directory and the
/// script the page consisted of, and the four after that hand an address to the desktop.
const RETIRED: &[&str] = &[
    "wasm-pack",
    "wasm-bindgen",
    "wasm32-",
    "trunk",
    "atlas-ui",
    "build-ui.sh",
    "public/ui",
    "public\\ui",
    "xdg-open",
    "sensible-browser",
    "webbrowser",
    "start-process",
    "open http",
    "open https",
    "open -a",
    "browser",
];

/// The repository root: `rust/atlas-setup` is two directories down from it.
fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::canonicalize(manifest.join("..").join(".."))
        .unwrap_or_else(|error| panic!("{} cannot be resolved: {error}", manifest.display()))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} cannot be read as text: {error}", path.display()))
}

/// Every `(needle, line number, line)` in `text` that names a retired capability.
///
/// The comparison is case-insensitive and the line number is 1-based, because a failure
/// has to name the line somebody edits next. `RETIRED` is the whole vocabulary: a hit here
/// is a step, a comment or a string that names the page this tree no longer has.
fn retired_hits(text: &str, forbidden: &[&str]) -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let lowered = line.to_lowercase();
        for needle in forbidden {
            if lowered.contains(needle) {
                hits.push(((*needle).to_string(), index + 1, line.trim().to_string()));
            }
        }
    }
    hits
}

/// The files under `dir` ending in `.rs`, deepest last.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().map(|ext| ext == "rs").unwrap_or(false) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// 1. the scripts name no web UI and open no page
// ---------------------------------------------------------------------------

/// Every script is there, so a rename cannot make the rest of this file pass by absence.
#[test]
fn the_eight_scripts_are_where_a_person_finds_them() {
    let root = root();
    for script in SCRIPTS {
        let path = root.join(script);
        assert!(
            path.is_file(),
            "{} is missing: the launcher scripts are how a fresh machine builds ATLAS, and \
             this file checks each one by name",
            path.display()
        );
        let text = read(&path);
        assert!(
            !text.trim().is_empty(),
            "{} is empty",
            path.display()
        );
    }
}

/// None of them names a wasm builder, the wasm target, the retired crate/directory/script,
/// or a call that opens a page.
#[test]
fn no_script_names_a_web_ui() {
    let root = root();
    let mut found = Vec::new();
    for script in SCRIPTS {
        let path = root.join(script);
        let text = read(&path);
        for (needle, line, content) in retired_hits(&text, RETIRED) {
            found.push(format!("{script}:{line} names {needle:?} — {content}"));
        }
    }
    assert!(
        found.is_empty(),
        "a launcher script still names the retired page:\n{}\n\
         the face of ATLAS is rust/target/release/atlas-desktop, an egui window that reads \
         data/atlas.db and opens no socket; a script that builds, copies or opens a page is \
         a second face nobody reviewed",
        found.join("\n")
    );
}

/// The scan is not blind: a planted script that copies the bundle, and one that opens an
/// address, are both findings.
///
/// This is the proof the rule can fail. It runs the predicates over a string rather than a
/// file, so it needs no fixture on disk and cannot be satisfied by a tree that happens to
/// be clean.
#[test]
fn the_scan_bites_when_a_ui_step_is_put_back() {
    let copied = "#!/bin/sh\ncp -R rust/atlas-ui/pkg public/ui\n";
    let hits = retired_hits(copied, RETIRED);
    assert!(
        !hits.is_empty(),
        "a script that copies the bundle was not a finding"
    );
    assert!(
        hits.iter().any(|(needle, _, _)| needle == "public/ui"),
        "the finding does not name the directory: {hits:?}"
    );

    let opened = "open https://127.0.0.1:8080/\n";
    assert!(
        !retired_hits(opened, RETIRED).is_empty(),
        "a script that opens an address was not a finding"
    );

    // And a clean line stays clean: the scan is a capability check, not a text tripwire
    // that fires on any sentence about the window.
    let clean = "say \"the window reads data/atlas.db and opens no socket\"\n";
    assert!(
        retired_hits(clean, RETIRED).is_empty(),
        "a sentence about the native window was read as a web UI"
    );
}

// ---------------------------------------------------------------------------
// 2. the setup scripts keep the two sentences a broken machine needs
// ---------------------------------------------------------------------------

/// The build command, and the sentence that says what to do when a piece is missing.
///
/// Removing a UI step must not remove the step that produces the window, nor the line that
/// turns "something is missing" into an action: `--provision` here, or the subcommand the
/// report itself named.
#[test]
fn setup_still_builds_the_window_and_says_what_to_do_about_a_missing_piece() {
    let root = root();
    for script in ["setup.sh", "setup.ps1"] {
        let text = read(&root.join(script));
        assert!(
            text.contains("cargo build --release --offline --locked"),
            "{script} no longer runs the offline build: the window has to come from \
             somewhere, and this is the one step that produces it"
        );
        assert!(
            text.contains("--provision") || text.contains("-Provision"),
            "{script} no longer tells a reader what to do when a piece is missing: the \
             sentence names the switch that provisions it"
        );
        assert!(
            text.to_lowercase().contains("idempotent"),
            "{script} no longer says it is idempotent: running it again after provisioning \
             something is the expected use, and the file has to say so"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. setup downloads no UI
// ---------------------------------------------------------------------------

/// The provisioning crate's download surface: every address it can fetch, and its own
/// dependency list.
///
/// `atlas-setup` is the one place in ATLAS where bytes arrive from the network (behind
/// `--yes`, for the model and the model server's source). A UI artifact in that surface
/// would be a run-time download of a page by another name, so the addresses are read here
/// rather than trusted: a `.wasm`, `.js`, `.html` or `.css` target, or an address naming
/// `atlas-ui`/`public/ui`, is a finding.
#[test]
fn setup_fetches_no_web_ui() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = crate_dir.join("src");
    let mut found = Vec::new();
    let mut addresses = 0usize;
    let ui_suffixes = [".wasm", ".js", ".html", ".css"];
    for file in rust_files(&src) {
        let text = read(&file);
        let name = file
            .strip_prefix(crate_dir)
            .unwrap_or(&file)
            .display()
            .to_string();
        for (index, line) in text.lines().enumerate() {
            let mut rest = line;
            while let Some(at) = rest.find("https://") {
                rest = &rest[at..];
                let end = rest
                    .find(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\\' || ch == ')')
                    .unwrap_or(rest.len());
                let address = &rest[..end];
                addresses += 1;
                let lowered = address.to_lowercase();
                if ui_suffixes.iter().any(|suffix| lowered.contains(suffix))
                    || lowered.contains("atlas-ui")
                    || lowered.contains("atlas_ui")
                    || lowered.contains("public/ui")
                {
                    found.push(format!("{name}:{} — {address}", index + 1));
                }
                rest = &rest[end..];
            }
        }
    }
    assert!(
        addresses > 0,
        "no address was read out of atlas-setup/src — the scan is blind, and \"no UI is \
         downloaded\" would be a claim about nothing"
    );
    assert!(
        found.is_empty(),
        "atlas-setup can fetch a web artifact:\n{}\n\
         provisioning fetches the model and the model server's source, both named by a \
         subcommand and both printed before they start; a page is neither",
        found.join("\n")
    );

    // The crate's own dependencies: a wasm or DOM crate here would be the same capability
    // arriving as a library rather than a URL.
    let manifest = read(&crate_dir.join("Cargo.toml"));
    for needle in ["wasm-bindgen", "wasm32", "web-sys", "js-sys", "atlas-ui"] {
        let lowered = manifest.to_lowercase();
        assert!(
            !lowered.contains(needle),
            "atlas-setup declares {needle:?}: the provisioning binary builds the model \
             server and fetches the model, and a web target is not one of the pieces"
        );
    }
}

/// Nothing under `public/ui/` is referenced by the scripts — the last question, asked of
/// the tree rather than of a person's memory.
///
/// The directory may still exist (it is generated: `rust/build-ui.sh` writes it), so this
/// does not assert its absence; it asserts that no script this file checks reads it, which
/// is what makes deleting it safe.
#[test]
fn no_script_reads_the_generated_bundle() {
    let root = root();
    for script in SCRIPTS {
        let text = read(&root.join(script));
        for needle in ["public/ui", "public\\ui"] {
            assert!(
                !text.to_lowercase().contains(needle),
                "{script} reads {needle}: the bundle is generated output of a retired page, \
                 so nothing here may depend on it being on disk"
            );
        }
    }
}
