//! audit_legacy_browser — the old CDP/Chrome reader must be provably dead in the product.
//!
//! The product used to read pages by starting a local Chrome and driving it over the
//! DevTools protocol. That whole path is gone: `Pipeline::native` wires the HTTP-only
//! reader, and no shipped binary links the crate that can produce a browser argv. A
//! removal is only worth something if it stays removed, so this file pins the removal
//! at four places, each of which is a different way for the old reader to come back:
//!
//! 1. **The manifests.** A dependency edge is how a crate gets a capability it does not
//!    spell in its own source. `atlas-browser` is not a dependency of the window, the
//!    agent, the monitor or the launcher — read straight from their `Cargo.toml`, and
//!    then walked transitively, because `ingest -> something -> atlas-browser` would
//!    be the same capability with one hop of deniability. The walk reads path edges and
//!    refuses to guess at `workspace = true` (see `closure`), so it cannot go quietly
//!    green the day the manifests are restructured.
//! 2. **The source that could launch a browser.** The launch surface has names: the
//!    crate that spawns Chrome also resolves the executable, builds its argv and reads
//!    the port file Chrome writes. None of those names appears in any shipped file.
//!    That is a stronger statement than "the word Chrome does not appear": `atlas-config`
//!    still defines a `chrome_profile` path and `atlas-platform` still knows where a
//!    browser *would* be installed, and both are allowed — discovery without a spawn is
//!    not a browser.
//! 3. **The port.** No shipped file reads the DevTools endpoint or the `cdp_port` dial.
//!    The dial itself is still *defined* in `atlas-config` (dead weight, see the report);
//!    a use of it anywhere outside that crate is the finding, because a port number is
//!    only interesting to something that connects to it.
//! 4. **The seam.** `atlas-ingest`'s shipped pipeline still constructs `NullBrowser` and
//!    the type that replaced it is gone from the source, so the agent's reader cannot
//!    quietly become a browser again without this file going red.
//!
//! Since this task the crate is gone as well: `atlas-browser`, `atlas-web` (its last
//! dependent) and `atlas-e2e` were deleted, so rule 1's walk has nothing to land on and
//! the workspace scan is no longer an absence in the shipped product only — it is the
//! whole tree. The tests that used the old manifests as *fixtures* for the detectors now
//! use synthetic manifests and the live manifests of crates that still have path edges;
//! each of those edits says why at the line. Tone: each failure names the file and line
//! and says what to do, so the next reader does not have to reconstruct the argument.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use atlas_guard::source::{shipped_code, SourceFile};

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

/// The workspace directory that holds the crates (`rust/`), which is what
/// `shipped_code` walks and where every `Cargo.toml` below lives.
fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Read one file of the tree, loudly. A missing manifest is a finding, not a skip:
/// a test that silently reads nothing is a test that passes for the wrong reason.
fn read(rust: &Path, dir: &str, file: &str) -> String {
    let path = if dir.is_empty() {
        rust.join(file)
    } else {
        rust.join(dir).join(file)
    };
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "cannot read {}: {error}. This audit reads the tree it ships in",
            path.display()
        )
    })
}

/// The crates whose shipped source a person's build links: the window, the agent, the
/// monitor, the launcher and everything under them. `atlas-browser`, `atlas-web`,
/// `atlas-e2e` and `atlas-ui` are not on this list — the first two are deleted and are
/// the thing this file exists to keep out, and the other two were the harness and the
/// wasm face, also deleted. `atlas-bench` is not shipped (a measurement harness), so its
/// source is out of scope here for a different reason.
const SHIPPED: &[&str] = &[
    "atlas-launcher",
    "atlas-desktop",
    "atlas-monitor",
    "atlas-ingest",
    "atlas-ai",
    "atlas-net",
    "atlas-store",
    "atlas-config",
    "atlas-setup",
    "atlas-platform",
    "atlas-core",
    "atlas-lib",
    "atlas-text",
    "atlas-topics",
    "atlas-places",
    "atlas-dates",
    "atlas-graph",
];

/// The four binaries a person starts. A transitive walk that starts anywhere else
/// (say at `atlas-core`) would prove little: these are what `atlas start` runs.
const BINARIES: &[&str] = &[
    "atlas-launcher",
    "atlas-desktop",
    "atlas-monitor",
    "atlas-ingest",
];

/// The crates a shipped build may not link. Both bring something the product is not
/// allowed to do: `atlas-browser` starts Chrome, `atlas-web` opens a listening socket.
const RETIRED: &[&str] = &["atlas-browser", "atlas-web"];

// ---------------------------------------------------------------------------
// 1. The manifests
// ---------------------------------------------------------------------------

/// The lines of `manifest` that would make `crate_name` a dependency: the key itself,
/// a `package = "…"` rename, or a `path = "../…"` edge. A comment is not an edge, and
/// is skipped — the workspace root explains the old design in prose and that prose is
/// not a dependency.
fn edge_hits(manifest: &str, crate_name: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (index, line) in manifest.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let key = format!("{crate_name} =");
        let key_spaced = format!("{crate_name}=");
        let renamed = format!("package = \"{crate_name}\"");
        let path = format!("path = \"../{crate_name}\"");
        if trimmed.starts_with(&key)
            || trimmed.starts_with(&key_spaced)
            || trimmed.contains(&renamed)
            || trimmed.contains(&path)
        {
            hits.push(format!("line {}: {}", index + 1, trimmed));
        }
    }
    hits
}

/// The crate directories a manifest points at with `path = "../x"`. Vendored trees and
/// `../..` hops are not workspace crates and are ignored by name, not by position.
fn path_dep_dirs(manifest: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.split("path = \"../").nth(1) else {
            continue;
        };
        let Some(dir) = rest.split('"').next() else {
            continue;
        };
        if dir.starts_with("atlas-") {
            out.insert(dir.to_string());
        }
    }
    out
}

/// Every workspace crate `start` links, transitively, one path edge at a time.
///
/// `workspace = true` is not followed, and an *internal* crate spelled that way panics
/// rather than being skipped: this walk must not be able to go green by not looking.
/// Measured today, internal edges are all `path = "…"` and third-party inheritance is
/// the only `workspace = true` in the tree, so the panic is a tripwire, not a shortcut.
fn closure(rust: &Path, start: &str) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = vec![start.to_string()];
    while let Some(current) = queue.pop() {
        let manifest = read(rust, &current, "Cargo.toml");
        for line in manifest.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('#') && trimmed.contains("workspace = true") {
                let key = trimmed.split('=').next().unwrap_or("").trim();
                assert!(
                    !key.starts_with("atlas-"),
                    "{current}/Cargo.toml inherits `{key}` from the workspace table; this \
                     walk follows `path = \"…\"` only and would miss that edge. Teach \
                     `closure` the spelling before trusting a green run"
                );
            }
        }
        for dep in path_dep_dirs(&manifest) {
            if dep != current && seen.insert(dep.clone()) {
                queue.push(dep);
            }
        }
    }
    seen
}

#[test]
fn no_shipped_manifest_asks_for_the_retired_browser() {
    let rust = rust_dir();
    let mut bytes = 0usize;
    for dir in SHIPPED {
        let manifest = read(&rust, dir, "Cargo.toml");
        assert!(
            manifest.contains(&format!("name = \"{dir}\"")),
            "{dir}/Cargo.toml does not declare `name = \"{dir}\"`. The walk is reading \
             the wrong file, so every empty result below is an artefact"
        );
        bytes += manifest.len();
        let hits = edge_hits(&manifest, "atlas-browser");
        assert!(
            hits.is_empty(),
            "{dir}/Cargo.toml names `atlas-browser`:\n{}\nThat crate is the one that \
             starts a Chrome with a remote-control port. The agent reads over plain \
             HTTP (`Pipeline::native` wires `NullBrowser`); an edge here is how the \
             browser comes back.",
            hits.join("\n")
        );
    }
    // The root table, because `workspace = true` is the way a member could inherit an
    // edge without its own manifest looking wrong.
    let root = read(&rust, "", "Cargo.toml");
    let table: Vec<&str> = root
        .lines()
        .skip_while(|line| line.trim() != "[workspace.dependencies]")
        .take_while(|line| !line.trim().starts_with('[') || line.trim() == "[workspace.dependencies]")
        .filter(|line| !line.trim().starts_with('#'))
        .collect();
    assert!(
        !table.is_empty(),
        "the root manifest has no `[workspace.dependencies]` table any more; this test \
         reads it to rule out an inherited edge, so it has to be taught the new layout"
    );
    let inherited = table
        .iter()
        .find(|line| line.contains("atlas-browser"))
        .map(|line| line.trim().to_string());
    assert!(
        inherited.is_none(),
        "the root manifest declares `atlas-browser` as a workspace dependency: {inherited:?}. \
         Any member could then link it with one `workspace = true` line"
    );
    println!(
        "read {} shipped manifests + the root table ({bytes} bytes): no edge to `atlas-browser`",
        SHIPPED.len()
    );
}

#[test]
fn the_shipped_binaries_link_no_browser_transitively() {
    let rust = rust_dir();
    let mut visits = 0usize;
    for root in BINARIES {
        let linked = closure(&rust, root);
        visits += linked.len();
        for retired in RETIRED {
            assert!(
                !linked.contains(*retired),
                "{root} reaches `{retired}` through path dependencies. Closure: {:?}. \
                 The window opens no socket and the agent starts no browser; remove the \
                 manifest edge that added this, not this assertion",
                linked
            );
        }
        // Non-vacuity, per root: the root's manifest was read, and where it declares
        // path edges the walk really followed them. The window declares none today (it
        // reads the archive through rusqlite in its own process), so an empty closure
        // there is a fact about the manifest, not a walker that stopped working — and
        // that distinction is exactly what the `walked` count below checks.
        let manifest = read(&rust, root, "Cargo.toml");
        assert!(
            manifest.contains(&format!("name = \"{root}\"")),
            "{root}/Cargo.toml does not declare `name = \"{root}\"`"
        );
        if path_dep_dirs(&manifest).iter().any(|dep| dep != root) {
            assert!(
                !linked.is_empty(),
                "the walk from {root} reached nothing although its manifest declares path \
                 dependencies. The walker is broken, not the tree"
            );
        }
    }
    let reached = BINARIES
        .iter()
        .filter(|root| !closure(&rust, root).is_empty())
        .count();
    assert!(
        reached >= 2,
        "only {reached} of {} shipped binaries resolved a single path dependency, so the \
         transitive half of this test has stopped walking anything",
        BINARIES.len()
    );
    // Non-vacuity: the walker really does follow edges. These are measured edges of the
    // agent's closure, and if any of them disappears the walk has stopped reading.
    let agent = closure(&rust, "atlas-ingest");
    for expected in ["atlas-store", "atlas-net", "atlas-ai", "atlas-platform"] {
        assert!(
            agent.contains(expected),
            "the walk from atlas-ingest does not reach `{expected}`, so it is not reading \
             the manifests. Closure: {agent:?}"
        );
    }
    let monitor = closure(&rust, "atlas-monitor");
    assert!(
        monitor.contains("atlas-ingest"),
        "atlas-monitor no longer links atlas-ingest, so the agent half of this walk is \
         empty. Closure: {monitor:?}"
    );
    println!(
        "walked {} shipped binaries ({visits} crate visits, {reached} roots with path \
         edges): no path edge to {RETIRED:?}",
        BINARIES.len()
    );
}

// ---------------------------------------------------------------------------
// 2. The source that could launch a browser
// ---------------------------------------------------------------------------

/// The names the launch surface cannot work without. Each one is a *different* piece of
/// the old reader, so a single hit is enough:
///
/// * `atlas_browser` — the crate's own API, the shortest path to a page.
/// * `find_executable`, `spawn_flags`, `EPHEMERAL_PORT`, `ws_target_loopback` — resolve
///   the browser, build its argv, name the port it writes, check where it listens.
/// * `DevToolsActivePort`, `Page.navigate`, `Runtime.evaluate` — read Chrome's port file
///   and speak the protocol (the port file is how a browser started on an *ephemeral*
///   port is found again; the flag that asks for one is spelled here as two pieces, at
///   the end of this list, so this audit does not itself carry the token).
/// * `--no-sandbox`, `--user-data-dir`, `--headless` — argv that only a browser takes.
/// * `lsof_verdict` — the loopback verdict the browser crate hands its callers.
///
/// `Chrome` as a word is deliberately not a needle: `atlas-config` explains the old
/// profile path and `atlas-platform` knows where a browser *would* be installed, and a
/// path lookup that nothing spawns is not a browser. The absence that matters is the
/// launch surface, not the vocabulary.
fn launch_needles() -> Vec<String> {
    let mut needles: Vec<String> = [
        "atlas_browser",
        "find_executable",
        "spawn_flags",
        "EPHEMERAL_PORT",
        "ws_target_loopback",
        "DevToolsActivePort",
        "Page.navigate",
        "Runtime.evaluate",
        "lsof_verdict",
        "--no-sandbox",
        "--user-data-dir",
        "--headless",
    ]
    .iter()
    .map(|needle| needle.to_string())
    .collect();
    // The one flag this file will not spell in one piece: the repository rule is that a
    // line naming it is a browser launch, comments included, and that rule is worth
    // keeping even in the audit that looks for it.
    needles.push(["--remote-", "debugging"].concat());
    needles
}

/// Every `src/*.rs` of the crates a shipped build links, i.e. the scan set of this file.
fn shipped_files(rust: &Path) -> Vec<SourceFile> {
    shipped_code(rust)
        .into_iter()
        .filter(|file| {
            SHIPPED
                .iter()
                .any(|name| file.path.starts_with(&format!("{name}/")))
        })
        .collect()
}

fn hits_of(files: &[SourceFile], needle: &str) -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for file in files {
        for (line, text) in file.hits(needle) {
            hits.push((file.path.clone(), line, text));
        }
    }
    hits
}

/// The argument of every `.with_devtools(…)` call in `code`, trimmed.
///
/// A scan and not a line filter, because the call may be written across lines and a
/// filter that only reads one line would quietly pass a call it never saw. `false` is
/// the only acceptable answer; the caller asserts that.
fn devtools_arguments(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(found) = code[from..].find("with_devtools(") {
        let start = from + found + "with_devtools(".len();
        let tail = &code[start..];
        let end = tail.find(')').unwrap_or(tail.len());
        // The trailing comma of a multi-line builder call is punctuation, not the value:
        // `.with_devtools(\n false,\n)` says `false`.
        out.push(tail[..end].trim().trim_end_matches(',').trim().to_string());
        from = start;
    }
    out
}

#[test]
fn no_shipped_source_can_launch_a_browser() {
    let rust = rust_dir();
    let files = shipped_files(&rust);
    assert!(
        files.len() > 120,
        "the scan read {} shipped files; that is too few for this tree, so the absence \
         below would prove nothing",
        files.len()
    );
    let needles = launch_needles();
    for needle in &needles {
        let hits = hits_of(&files, needle);
        assert!(
            hits.is_empty(),
            "`{needle}` appears in shipped code, so the product can still drive a \
             browser:\n{}\nThe shipped reader is `NullBrowser` + plain HTTP \
             (`atlas-ingest/src/lib.rs`, `Pipeline::native`). If a page really needs a \
             rendering engine, that decision belongs to `atlas-desktop::webview`, which \
             runs the system web view in-process and opens no port — not to a spawned \
             browser and not to this crate.",
            hits.iter()
                .map(|(path, line, text)| format!("{path}:{line}: {text}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    // A spawn call that names a browser on its own line: `Command::new("…chrome…")`.
    // `atlas-platform::child` is the one spawn helper the app has and its browser
    // mentions live in prose, so a same-line hit is a real launch and not a comment.
    let mut same_line: Vec<String> = Vec::new();
    for file in &files {
        for (index, line) in file.code.lines().enumerate() {
            if line.contains("Command::new") && line.to_lowercase().contains("chrom") {
                same_line.push(format!("{}:{}: {}", file.path, index + 1, line.trim()));
            }
        }
    }
    assert!(
        same_line.is_empty(),
        "a spawn call names a browser:\n{}",
        same_line.join("\n")
    );

    // Non-vacuity 1: the same scan reads the product's real text. `atlas-ai` speaks to
    // the local model over loopback, so the control needle has to be there.
    assert!(
        hits_of(&files, "127.0.0.1")
            .iter()
            .any(|(path, _, _)| path.starts_with("atlas-ai/")),
        "the scan found no `127.0.0.1` in atlas-ai's source, so it is reading the wrong \
         tree and every empty result above is an artefact"
    );
    // Non-vacuity 2: each needle is a live needle. This used to read the text of
    // `atlas-browser/src` — the crate that drove a browser — and assert every needle
    // occurred there. That crate was deleted with the web face (this task), so the
    // fixture is gone. What is asserted instead is the half that can still be proved
    // without putting the old reader back: every needle is a real string that the
    // matcher finds in a sample of the launch surface it was written to catch. A typo'd
    // needle would return nothing from this sample, and the empty result above would
    // then mean nothing.
    let sample = vec![SourceFile {
        path: "sample/browser.rs".to_string(),
        code: concat!(
            "use atlas_browser::{find_executable, spawn_flags, ws_target_loopback};\n",
            "const EPHEMERAL_PORT: u16 = 0;\n",
            "let port = read_devtools_active_port(); // DevToolsActivePort\n",
            "let flags = [\"--headless\", \"--no-sandbox\", \"--user-data-dir=/tmp/p\"];\n",
            "let _ = codex(\"Page.navigate\");\n",
            "let _ = codex(\"Runtime.evaluate\");\n",
            "let _ = lsof_verdict(pid);\n",
            "let _ = [\"--remote-\", \"debugging\", \"=9222\"].concat();\n",
        )
        .to_string(),
    }];
    // The `--remote-debugging` needle is spelled in two halves on purpose (see
    // `launch_needles`): a line holding the whole token is a browser launch by this
    // repository's rule, and this audit will not write one either. Its halves are
    // checked against the sample instead, which proves the two strings are the ones that
    // occur in a launch and that the concatenation is what the scan looks for.
    let split = ["--remote-", "debugging"].concat();
    for needle in needles.iter().filter(|needle| **needle != split) {
        assert!(
            !hits_of(&sample, needle).is_empty(),
            "`{needle}` was not found in the launch-surface sample, so it is a typo'd \
             needle and the empty result above is meaningless"
        );
    }
    for half in ["--remote-", "debugging"] {
        assert!(
            !hits_of(&sample, half).is_empty(),
            "`{half}` was not found in the launch-surface sample, so the split needle \
             `{split}` is built from strings no launch holds"
        );
    }
    println!(
        "scanned {} shipped files for {} launch-surface names: none found",
        files.len(),
        needles.len()
    );
}

// ---------------------------------------------------------------------------
// 3. The port
// ---------------------------------------------------------------------------

#[test]
fn no_cdp_port_is_read_outside_the_browser_crate() {
    let rust = rust_dir();
    let files = shipped_files(&rust);
    // Nothing but the browser crate may name the endpoint or read Chrome's port file.
    for needle in [
        "DevToolsActivePort",
        "webSocketDebuggerUrl",
        "Page.navigate",
        "Runtime.evaluate",
        "json/version",
        "lsof_verdict",
    ] {
        let hits = hits_of(&files, needle);
        assert!(
            hits.is_empty(),
            "`{needle}` is a DevTools endpoint, not something the product needs:\n{}\n\
             The agent reads pages over plain HTTP; the window renders in its own \
             process. A port number is only interesting to something that connects to \
             it, so this is the first half of a browser coming back.",
            hits.iter()
                .map(|(path, line, text)| format!("{path}:{line}: {text}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    // The dial itself is still *defined* in `atlas-config` (the setting, its default and
    // its validator). That is allowed and is recorded as dead weight rather than as a
    // dependency: nothing shipped reads it. What may not happen is a *use* elsewhere.
    let mut uses: Vec<String> = Vec::new();
    for file in &files {
        if file.path.starts_with("atlas-config/") {
            continue;
        }
        for (line, text) in file.hits("cdp_port") {
            uses.push(format!("{}:{line}: {text}", file.path));
        }
    }
    assert!(
        uses.is_empty(),
        "`cdp_port` is read outside atlas-config:\n{}\nThe dial configures a port only \
         a browser listens on; the shipped product opens no such port, so a reader here \
         is a connection nobody should be able to make",
        uses.join("\n")
    );
    // The window's own page view is the one thing in the product that embeds an engine.
    // It must not expose a debug console: `.with_devtools(false)` is the whole claim.
    if read(&rust, "atlas-desktop", "Cargo.toml").contains("wry") {
        let mut sites = 0usize;
        for file in &files {
            for argument in devtools_arguments(&file.code) {
                assert_eq!(
                    argument, "false",
                    "{}:`.with_devtools({argument})` turns on a debug console for a page \
                     the reader did not write. The window ships with it off",
                    file.path
                );
                sites += 1;
            }
        }
        assert!(
            sites > 0,
            "the window links `wry` but never states `.with_devtools(false)`, so whether a \
             page can be debugged is left to the engine's default. Say it explicitly"
        );
        println!("window page view: {sites} devtools decision(s), every one `false`");
    }
    println!("no shipped file reads a CDP endpoint or the `cdp_port` dial");
}

// ---------------------------------------------------------------------------
// 4. The seam
// ---------------------------------------------------------------------------

/// The text between `pub fn native(` and the next method, i.e. the body of the function
/// that decides what the shipped pipeline reads pages with.
fn native_body(lib: &str) -> &str {
    let start = lib.find("pub fn native(").unwrap_or_else(|| {
        panic!(
            "atlas-ingest/src/lib.rs has no `pub fn native(`: the shipped pipeline \
             constructor moved or was renamed, and this audit no longer knows where the \
             reader is decided"
        )
    });
    let tail = &lib[start..];
    let end = tail.find("\n    pub fn ").unwrap_or(tail.len());
    &tail[..end]
}

#[test]
fn the_pipeline_still_wires_the_http_only_reader() {
    let rust = rust_dir();
    let lib = read(&rust, "atlas-ingest", "src/lib.rs");
    let fetcher = read(&rust, "atlas-ingest", "src/fetcher.rs");
    let read_path = read(&rust, "atlas-ingest", "src/read.rs");
    let manifest = read(&rust, "atlas-ingest", "Cargo.toml");

    let body = native_body(&lib);
    assert!(
        body.contains("Arc::new(NullBrowser)"),
        "`Pipeline::native` no longer constructs `NullBrowser`:\n{body}\nThat was the \
         decision that took the browser out of the agent. If the reader moved to \
         another constructor, point this assertion at the new one — do not delete it"
    );
    assert!(
        !body.contains("ChromeBrowser"),
        "`Pipeline::native` names `ChromeBrowser` again:\n{body}"
    );
    assert!(
        !lib.contains("ChromeBrowser") && !fetcher.contains("ChromeBrowser"),
        "the shipped ingest source still mentions `ChromeBrowser`; the type was deleted \
         with the seam, so a mention is either a resurrection or a stale import"
    );
    assert!(
        fetcher.contains("pub struct NullBrowser"),
        "atlas-ingest/src/fetcher.rs no longer defines `NullBrowser`, so the shipped \
         reader has no HTTP-only implementation"
    );
    assert!(
        lib.contains("NullBrowser") && lib.contains("pub use fetcher::"),
        "`NullBrowser` is not re-exported from atlas-ingest, so a caller cannot name the \
         reader the product ships with"
    );
    assert!(
        edge_hits(&manifest, "atlas-browser").is_empty(),
        "atlas-ingest/Cargo.toml names `atlas-browser`; the pipeline would then be one \
         `ChromeBrowser::global()` away from a listener"
    );
    // The honest degrade: a page that needs JavaScript is a sentence the model can act
    // on, and the reason code the tools report stays `js_only`. Pinned here because a
    // silent empty read is the failure mode this whole change could otherwise hide.
    assert!(
        read_path.contains("js_only"),
        "atlas-ingest/src/read.rs no longer reports the `js_only` reason, so a page that \
         needs JavaScript would read as an empty page instead of as a page this build \
         cannot render"
    );
    assert!(
        read_path.contains("no browser"),
        "atlas-ingest/src/read.rs no longer says that no browser runs here. The sentence \
         is the honest half of removing the reader: the model has to know the text is \
         missing because nothing rendered it"
    );
    println!("pipeline seam: `Pipeline::native` -> `NullBrowser`, `ChromeBrowser` absent, `js_only` intact");
}

// ---------------------------------------------------------------------------
// 5. The detectors themselves
// ---------------------------------------------------------------------------

/// A green absence is only worth something if the detector behind it is not blind. This
/// test runs each detector against input that *does* carry the old reader — the two
/// manifests in this tree that really do name `atlas-browser` today, plus synthetic
/// snippets shaped like the code that was deleted — and asserts it reports them.
///
/// It is the honest half of "this test would have failed on the old code": the four
/// tests above cannot be run against a tree that still has the browser without putting
/// it back (which would be a change to files other agents own). What can be shown here
/// is that each detector returns a finding for exactly that input.
#[test]
fn the_detectors_catch_the_old_reader() {
    let rust = rust_dir();

    // 1. The manifest detector, on a synthetic manifest that carries the edge. This used
    //    to read `atlas-web/Cargo.toml` and `atlas-e2e/Cargo.toml`, the two live files
    //    that named `atlas-browser`; both crates were deleted (this task), so the fixture
    //    is a manifest written here, holding the same three spellings the detector looks
    //    for — key, rename, path edge — plus a commented-out edge it must ignore.
    let manifest = "# atlas-browser = { path = \"../atlas-browser\" }\n\
                    [dependencies]\n\
                    atlas-browser = { path = \"../atlas-browser\" }\n\
                    chrome = { package = \"atlas-browser\", path = \"../atlas-browser\" }\n";
    let hits = edge_hits(manifest, "atlas-browser");
    assert_eq!(
        hits.len(),
        2,
        "the manifest detector found {hits:?} in a manifest with one edge and one rename \
         (and one commented-out edge it must ignore), so the manifest half of every test \
         above is blind"
    );
    assert!(
        hits.iter().all(|hit| hit.contains("atlas-browser")),
        "the detector reported a line that does not name the crate: {hits:?}"
    );
    assert!(
        edge_hits("# atlas-browser = { path = \"../atlas-browser\" }", "atlas-browser")
            .is_empty(),
        "a commented-out edge is not an edge, and the detector thinks it is"
    );

    // 2. The walk, on a crate that still has path edges. This used to start at
    //    `atlas-web`, which reached `atlas-browser` in one hop; that crate is gone. The
    //    shape being proved is the same one: a walker that follows `path = …` edges lands
    //    on a crate the root does not name itself, and never reports the root.
    let ingest = closure(&rust, "atlas-ingest");
    assert!(
        ingest.contains("atlas-store"),
        "the closure of atlas-ingest did not reach `atlas-store`, so the walker follows \
         nothing: {ingest:?}"
    );
    assert!(
        !ingest.contains("atlas-ingest"),
        "the walker reported its own root as a dependency: {ingest:?}"
    );

    // 3. The source detector. One synthetic file, one real needle.
    let fake = SourceFile {
        path: "fake/src/spawn.rs".to_string(),
        code: "let flags = spawn_flags(EPHEMERAL_PORT, profile);\n".to_string(),
    };
    let hits = hits_of(std::slice::from_ref(&fake), "spawn_flags");
    assert_eq!(
        hits.len(),
        1,
        "`hits_of` did not report a line it was given: {hits:?}"
    );
    assert_eq!(hits[0].0, "fake/src/spawn.rs");
    assert_eq!(hits[0].1, 1);

    // 4. The pipeline extractor, on the wiring this audit exists to prevent. It has to
    //    see the browser and stop at the end of the function, or a hit in the next
    //    method would be blamed on `native`.
    let old = "impl Pipeline {\n    pub fn native(store: Arc<Store>) -> Result<Pipeline, E> {\n        Ok(Pipeline::new(Arc::new(ChromeBrowser::global())))\n    }\n\n    pub fn store(&self) -> &Arc<Store> {\n        &self.store\n    }\n}\n";
    let body = native_body(old);
    assert!(
        body.contains("ChromeBrowser"),
        "the `native` extractor did not see a browser wiring it was given: {body:?}"
    );
    assert!(
        !body.contains("pub fn store"),
        "the `native` extractor ran past the end of the function: {body:?}"
    );

    // 5. The devtools detector, both shapes a call can take.
    assert_eq!(devtools_arguments(".with_devtools(true)"), vec!["true"]);
    assert_eq!(
        devtools_arguments(".with_devtools(\n                false,\n            )"),
        vec!["false"]
    );
    assert_eq!(
        devtools_arguments("let x = 1; // nothing to see"),
        Vec::<String>::new()
    );

    println!("every detector reports the old reader when it is given one");
}
