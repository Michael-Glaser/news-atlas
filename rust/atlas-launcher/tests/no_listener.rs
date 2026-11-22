//! `atlas start` opens no port and starts no browser — measured, not asserted in prose.
//!
//! Three claims, and each one is checked from the outside of the crate, on files and on
//! the public functions a user can reach:
//!
//! 1. **The plan says so.** `atlas start --dry-run` prints the sequence: the model
//!    server, then the window, "no listener", the stop order — and exits 0 without
//!    starting anything. A regression that reintroduces a listener has to delete a line
//!    this test names.
//! 2. **The closure says so.** Walking `Cargo.toml` from this crate through its path
//!    dependencies reaches five workspace crates, none of which is `atlas-web` (the
//!    only listener), `atlas-browser` (the only Chrome launcher), `atlas-ingest` (the
//!    fetcher) or `atlas-net` (the HTTP client). `atlas-guard/tests/reachability.rs`
//!    holds the workspace-wide version of this rule; this one is the launcher's own, so
//!    a break shows up in the crate that broke it.
//! 3. **The shipped source says so.** Production code in this closure never accepts a
//!    connection (`accept(`), never names the DevTools flag, never names
//!    `ChromeBrowser`, and this crate's own production code never mentions
//!    `TcpListener` at all.
//!
//! `atlas-config` asks the kernel whether a port is free by binding it and dropping it in
//! one statement (`atlas-config/src/ports.rs:64`). That is a probe, not a listener: it
//! never accepts, and the test below is written as "nothing accepts" for exactly that
//! reason.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The directory of one workspace member, from its manifest.
fn member_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate is inside rust/")
        .join(name)
}

/// One manifest's *linked* path dependencies: the crates it carries into whatever it
/// builds.
///
/// Line-based on purpose: this workspace spells a member edge as
/// `atlas-x = { path = "../atlas-x" }`, and a `toml` dependency for a test that reads
/// five manifests would be a dependency the product does not need.
fn path_deps(manifest: &Path) -> Vec<String> {
    path_deps_in(&std::fs::read_to_string(manifest).unwrap_or_default())
}

/// The parse behind [`path_deps`], on text, so the section filter can be pinned without
/// writing a manifest to disk.
///
/// Section-aware on purpose, and it is a measured correction rather than a preference.
/// `atlas-guard/Cargo.toml:24` declares `atlas-desktop` under `[dev-dependencies]`, because
/// `atlas-guard/tests/audit_webview.rs` has to call the window's own predicate instead of
/// copying it. Cargo never builds a dependency's dev-dependencies, so that edge cannot put
/// a line of window code into the `atlas` binary — but a section-blind walk of every
/// `path = "..."` line reported `atlas-desktop` in this closure and failed this test on a
/// change that shipped nothing. The rule is about what is *linked*: `[dependencies]` and
/// the per-target spelling `[target.'cfg(...)'.dependencies]` count, `[dev-dependencies]`
/// and `[build-dependencies]` do not — for a dependency, and equally for this crate as the
/// root of the walk, since the shipped binary links neither of those either.
fn path_deps_in(text: &str) -> Vec<String> {
    let mut deps: Vec<String> = Vec::new();
    // Is the cursor inside a table Cargo links? `Some(name)` when the table *is* one
    // dependency (`[dependencies.atlas-x]`), `None` when it is not a dependency table
    // (`[package]`, `[lib]`, `[features]`).
    let mut linked: Option<Option<String>> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let header = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            linked = match header.rsplit('.').next().unwrap_or(&header) {
                "dependencies" => Some(None),
                "dev-dependencies" | "build-dependencies" => None,
                name if header.starts_with("dependencies.") && name.starts_with("atlas-") => {
                    Some(Some(name.to_string()))
                }
                _ => None,
            };
            continue;
        }
        match &linked {
            // `[dependencies.atlas-x]` + `path = "../atlas-x"`.
            Some(Some(name)) => {
                if line.starts_with("path") && !deps.iter().any(|dep| dep == name) {
                    deps.push(name.clone());
                }
            }
            Some(None) => {
                if !line.contains("path = ") {
                    continue;
                }
                let Some((key, _)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                if key.starts_with("atlas-") && !deps.iter().any(|dep| dep == key) {
                    deps.push(key.to_string());
                }
            }
            None => {}
        }
    }
    deps
}

/// Every workspace crate this binary links, transitively, plus itself.
fn closure() -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = vec!["atlas-launcher".to_string()];
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let manifest = member_dir(&name).join("Cargo.toml");
        for dep in path_deps(&manifest) {
            if !seen.contains(&dep) {
                queue.push(dep);
            }
        }
    }
    seen
}

/// The production half of one crate's sources: `(file, code)` for every `.rs` file under
/// `src/`, with test modules cut out and test-only files dropped.
fn shipped_sources(crate_name: &str) -> Vec<(String, String)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };
            if entry.path().is_dir() {
                walk(&entry.path(), &rel, out);
                continue;
            }
            if !rel.ends_with(".rs") {
                continue;
            }
            let stem = rel.rsplit('/').next().unwrap_or(&rel).trim_end_matches(".rs");
            // A file called `tests.rs` is a `#[cfg(test)] mod tests;` from somewhere —
            // everything in it is a test, and a test may hold a socket.
            if stem == "tests" {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let shipped = match text.find("\n#[cfg(test)]") {
                Some(at) => text[..at].to_string(),
                None => text,
            };
            // Comments go too: a sentence that explains what was removed ("this used to
            // serve `atlas-web`") names the crate without linking it, and the rule here
            // is about code.
            let code: String = shipped
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<&str>>()
                .join("\n");
            out.push((rel, code));
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    walk(&member_dir(crate_name).join("src"), "", &mut out);
    out
}

/// What production code must never spell, in any crate of this binary's closure.
const FORBIDDEN: [&str; 8] = [
    "--remote-debugging-port",
    "ChromeBrowser",
    "atlas_browser",
    "atlas_web",
    "accept(",
    "axum::",
    "hyper::",
    "devtools/browser",
];

#[test]
fn the_closure_of_this_binary_holds_no_listener_and_no_browser() {
    let closure = closure();
    let expected: BTreeSet<String> = [
        "atlas-config",
        "atlas-guard",
        "atlas-launcher",
        "atlas-platform",
        "atlas-store",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
        closure, expected,
        "the launcher's reachable member set changed. It is the one shipped binary that \
         starts a process, and it must reach no listener (atlas-web), no Chrome launcher \
         (atlas-browser), no fetcher (atlas-ingest) and no HTTP client (atlas-net). If an \
         edge is genuinely needed, name it here and say in the manifest why it cannot serve."
    );

    // The named exclusions, so a failure says which promise broke.
    for forbidden in [
        "atlas-web",
        "atlas-browser",
        "atlas-ingest",
        "atlas-net",
        "atlas-monitor",
    ] {
        assert!(
            !closure.contains(forbidden),
            "atlas-launcher reaches `{}` — the window, the agent and the model server are \
             all this binary starts, and each one is a separate program it spawns, not a \
             crate it links",
            forbidden
        );
    }

    let mut sites: Vec<String> = Vec::new();
    for name in &closure {
        for (file, code) in shipped_sources(name) {
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    sites.push(format!("{}::{} names {:?}", name, file, needle));
                }
            }
        }
    }
    assert!(
        sites.is_empty(),
        "a crate in this binary's closure names a listener or a browser: {:#?}",
        sites
    );
}

#[test]
fn the_stop_order_is_the_window_then_the_agent_then_the_model_server() {
    // The order a reader reads in the plan and the order the shutdown performs are the
    // same three words, and the agent is in the middle for a reason: it drives research
    // passes through the model, so it is asked to stop while the model can still answer.
    let order: Vec<&str> = atlas_launcher::shutdown::ORDER
        .iter()
        .map(|step| step.name())
        .collect();
    assert_eq!(order, vec!["window", "agent", "model server"]);
    assert_eq!(atlas_launcher::shutdown::ORDER.len(), 3);
}

#[test]
fn this_crate_never_binds_a_port_at_all() {
    // Stricter than the closure rule: `atlas-config` binds a port for one statement to
    // ask whether it is free, and that is allowed there. Here it is not allowed at all —
    // `atlas start` is the process a user runs, and it opens nothing.
    let mut offenders: Vec<String> = Vec::new();
    for (file, code) in shipped_sources("atlas-launcher") {
        for needle in ["TcpListener", "TcpServer", "bind((", "UdpSocket"] {
            if code.contains(needle) {
                offenders.push(format!("{}: {}", file, needle));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "atlas-launcher's shipped code binds something: {:#?}",
        offenders
    );
}

#[test]
fn the_dry_run_prints_a_plan_with_no_listener_and_no_browser() {
    let root = std::env::temp_dir().join(format!(
        "atlas-dry-run-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0)
    ));
    let _ = std::fs::create_dir_all(root.join("data"));
    let paths = atlas_config::Paths::at(&root);
    // Three stand-in binaries in this root's own `target/release`, so the plan prints the
    // *found* form of every child line whatever this machine has installed: the resolver
    // is `atlas-platform`'s, and what this test reads is the line a reader gets. The
    // install tool is one of them because the plan names it too — as a child, exactly
    // like the window and the agent, which is what keeps it out of this binary's closure.
    let release = root.join("rust").join("target").join("release");
    std::fs::create_dir_all(&release).expect("release dir");
    for name in ["atlas-desktop", "atlas-monitor", "atlas-setup"] {
        let path = atlas_platform::host::app_binary(&release, name);
        std::fs::write(&path, b"#!/bin/sh\n").expect("a stand-in binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = std::fs::metadata(&path).expect("mode").permissions();
            mode.set_mode(0o755);
            std::fs::set_permissions(&path, mode).expect("chmod");
        }
    }
    let config = atlas_config::Config::load_at(&paths).ok();
    let lines = atlas_launcher::plan::plan_lines(&paths, config.as_ref(), true);
    let text = lines.join("\n");

    // The plan's first line after the root is the provisioning step: it is the one step
    // that can touch the network, so a reader must be able to see it before anything
    // else, and this root has neither a model nor a model server.
    let provision = text
        .find("provisioning ")
        .expect("the plan names the provisioning step");
    let root_at = text.find("root ").expect("the plan names the root");
    assert!(
        root_at < provision,
        "the provisioning step is not the first step in the plan:\n{text}"
    );
    assert!(
        text.contains("atlas-setup --install"),
        "the plan does not name the install child and its argument:\n{text}"
    );
    assert!(
        text.contains("touches the network"),
        "the plan does not say the provisioning step is the network use:\n{text}"
    );

    // The plan's lines are in the order the boot does them — the sentence above
    // `plan_lines` — and that is measured on both sides here, because the two disagreed:
    // the child lines used to print window, agent, model server (the *stop* order, the
    // reverse of the spawn order) under that sentence. `model file` is a fourth line and is
    // not a child, and the searches carry a trailing space so "model file " cannot match
    // the "model server " line.
    // By *line*, not by substring: the provisioning line names "the model file (...)" in
    // its sentence, and a `text.find` found that one first and saw boot order in a plan that
    // printed the stop order (measured: this assertion passed on the old order until it was
    // line-anchored).
    let line_of = |label: &str| -> usize {
        text.lines()
            .position(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("the plan has no line starting with {label:?}:\n{text}"))
    };
    let plan_order: Vec<&str> = {
        let mut at: Vec<(&str, usize)> = vec![
            ("model file", line_of("model file ")),
            ("window", line_of("window ")),
            ("agent", line_of("agent ")),
            ("model server", line_of("model server ")),
        ];
        at.sort_by_key(|(_, offset)| *offset);
        at.into_iter().map(|(name, _)| name).collect()
    };
    let boot_order: Vec<&str> = {
        // The same four steps in `start::run`, by the call that performs each one.
        let boot = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/start.rs"))
            .expect("the boot is readable");
        let body = boot
            .split_once("pub fn run(")
            .map(|(_, rest)| rest.to_string())
            .expect("start::run exists");
        let mut at: Vec<(&str, usize)> = [
            ("model file", "model::resolve("),
            ("window", "window::binary("),
            ("agent", "agent::binary("),
            ("model server", "start_model("),
        ]
        .iter()
        .map(|(name, call)| {
            let offset = body
                .find(call)
                .unwrap_or_else(|| panic!("the boot no longer calls {call}"));
            (*name, offset)
        })
        .collect();
        at.sort_by_key(|(_, offset)| *offset);
        at.into_iter().map(|(name, _)| name).collect()
    };
    assert_eq!(
        plan_order, boot_order,
        "the plan and the boot disagree about the order of the same four steps:\n{text}"
    );
    assert!(
        text.contains("atlas-desktop"),
        "the plan names the window binary:\n{}",
        text
    );
    assert!(
        text.contains("atlas-monitor serve"),
        "the plan names the agent binary and its subcommand:\n{}",
        text
    );
    assert!(
        text.contains("one JSON line per action"),
        "the plan says what the agent prints, and that the launcher parses none of it:\n{}",
        text
    );
    // …and says what it does not do, in the words a reader came for.
    assert!(text.contains("no listener"), "{}", text);
    assert!(text.contains("no browser"), "{}", text);
    assert!(
        text.contains("stop order: window, then agent, then model server"),
        "{}",
        text
    );
    // Nothing in the plan promises an address for a server of ours, and nothing offers
    // to open a browser. The only URL-shaped text allowed is the model's own port.
    for stale in [
        "ATLAS running locally",
        "http://127.0.0.1:8787",
        "open http://",
        "--no-open",
        "ATLAS_OPEN",
    ] {
        assert!(!text.contains(stale), "the plan still promises {:?}:\n{}", stale, text);
    }

    // And the command a user types prints it and exits 0 without starting anything.
    let code = atlas_launcher::main_entry(&[
        "start".to_string(),
        "--dry-run".to_string(),
        "--root".to_string(),
        root.display().to_string(),
    ]);
    assert_eq!(code, 0, "a dry run is not a failure");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_stop_needs_no_runtime_for_a_test_to_use_it() {
    // `signals::install` is the only constructor the boot calls, and it wants a tokio
    // handle and installs a real signal handler — fine inside the crate, useless from
    // here: this file is a separate crate, so an item behind `#[cfg(test)]` does not
    // exist for it at all. The two plain constructors are part of the API for exactly this
    // question, "what does a step do when nothing asked it to stop, and when a `SIGTERM`
    // got there first", and this assertion is what keeps them out of `cfg(test)`: put the
    // attribute back and this file stops compiling.
    assert!(
        !atlas_launcher::signals::Stop::idle().requested(),
        "an idle stop must read as not requested"
    );
    assert!(
        atlas_launcher::signals::Stop::fired().requested(),
        "a fired stop must read as requested"
    );
}

#[test]
fn a_dependency_the_binary_does_not_link_is_not_in_its_closure() {
    // The parse, on a manifest written here, one line per spelling that exists in this
    // workspace today. The two build-time tables are what made this test red on a change
    // that shipped nothing (`atlas-guard`'s dev-dependency on the window); a real runtime
    // edge into a listener still has to be counted, which is the first line below.
    let manifest = r#"
[package]
name = "atlas-x"

[lib]
path = "src/lib.rs"

[dependencies]
atlas-web = { path = "../atlas-web" }
# atlas-browser = { path = "../atlas-browser" }

[dev-dependencies]
atlas-desktop = { path = "../atlas-desktop" }

[build-dependencies]
atlas-ingest = { path = "../atlas-ingest" }

[target.'cfg(unix)'.dependencies]
atlas-net = { path = "../atlas-net" }

[dependencies.atlas-graph]
path = "../atlas-graph"
"#;
    assert_eq!(
        path_deps_in(manifest),
        ["atlas-web", "atlas-net", "atlas-graph"],
        "the closure is the linked set: runtime edges in either spelling, and neither the \
         commented-out edge nor a dev- or build-dependency, which Cargo never puts in the \
         binary"
    );

    // And the measurement that made the filter necessary, on the shipped manifest: the
    // guard declares the window for its own test, and the launcher's closure has no window
    // in it — the launcher *spawns* `atlas-desktop` and links no line of it.
    let guard = member_dir("atlas-guard").join("Cargo.toml");
    assert!(
        !path_deps(&guard).contains(&"atlas-desktop".to_string()),
        "atlas-guard's dev-dependency on the window is being counted as a linked crate"
    );
    assert!(
        !closure().contains("atlas-desktop"),
        "the launcher links the window crate instead of spawning its binary: {:?}",
        closure()
    );
}
