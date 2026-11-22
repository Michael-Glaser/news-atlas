//! Reachability from the two shipped binaries — the no-port rule, walked not grepped.
//!
//! The user's hard rule is that ATLAS must never open a port and that the product
//! is one native window with no browser machinery. That rule is not broken by a
//! string in this tree: `atlas-web` really does `bind` a loopback socket and
//! `atlas-browser` really does start a Chrome that writes its DevTools port into
//! `DevToolsActivePort`. The rule holds because **no dependency edge connects the
//! window to either of them**, and an edge that does not exist cannot be reached at
//! run time. This test walks the manifests and says so.
//!
//! # Why a walk, and why over the manifests
//!
//! A grep answers "where is this written"; the question here is "can this shipped
//! binary reach it", and the compiler links exactly what each crate's
//! `[dependencies]` says, transitively. So the graph is the manifests: one node per
//! workspace member, one edge per workspace dependency (a `path` dep, or an
//! inherited `workspace = true` path dep, or a bare name that happens to be a
//! member). External crates sit at the leaves of this walk because they cannot
//! depend on a member of this workspace — they would have to name it by path, and a
//! crates.io manifest cannot.
//!
//! `[build-dependencies]` counts as a shipped edge: a build script is compiled and
//! *run* during `cargo build`, so a path dep that only appears there is still a
//! crate the shipped thing executes. `[dev-dependencies]` does not: it is compiled
//! into the test harness, never into the binary, and a test may bind
//! `127.0.0.1:0` inside its own process (that is the one exemption the rules allow).
//!
//! # What is scanned
//!
//! `atlas_guard::source::shipped_code` decides what shipped code is: `src/**/*.rs`
//! with `#[cfg(test)]` blocks blanked, test files skipped, and a module the crate
//! declares only under `#[cfg(test)]` (like `atlas-net/src/server.rs`, the loopback
//! server its own tests use) left out. Every hit is matched with the wrapped
//! matcher, so `TcpListener ::` + newline + `bind(` and a flag built with
//! `format!("--remote-debugging-port={}", port)` are both found.
//!
//! # What this test does not claim
//!
//! * It does not say `atlas-web`/`atlas-browser` are safe. They are reachable from
//!   `atlas` (`atlas-launcher`), which serves the local API and starts the reader
//!   browser by design; `audit_binds.rs` proves every one of those sockets is a
//!   loopback literal. This test is the *other* half: which binary can reach them.
//! * It does not audit the crates.io leaves of the window's graph (eframe, winit,
//!   glow, rusqlite). Those open an OS window and a GPU surface, which is the
//!   product; whether one of them contains an HTTP client is the lock/manifest
//!   audits' claim, and §4.14 of `docs/THREAT-MODEL.md` says what that exclusion
//!   costs.
//! * It does not read `Cargo.lock`. The lock names resolved versions; the edges a
//!   binary is built from are the manifests. `audit_manifest.rs` and
//!   `audit_freshness.rs` own the lock.
//!
//! # How each rule is proved to have teeth
//!
//! Every rule below is a small pure function over data, so the live test and its
//! planted regression are the same code: the live assertion runs it on the real
//! workspace, the planted one runs it on a hostile synthetic workspace or source
//! text that *does* connect the window to `atlas-browser`, and asserts the rule
//! reports it. The plants are synthetic rather than a temporary edit of a real
//! `Cargo.toml` on purpose: those manifests belong to other agents working in this
//! tree right now, and a plant that changes `atlas-desktop`'s dependency list would
//! also change what their builds link — the check would prove a rule at the cost of
//! somebody else's compile.

use atlas_guard::source::{find_spanned, shipped_code, SourceFile};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

// ---------------------------------------------------------------------------
// The manifest walk
// ---------------------------------------------------------------------------

/// Which table a dependency was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Package,
    /// `[dependencies]` — linked into the library or binary.
    Link,
    /// `[build-dependencies]` — compiled *and run* during the build.
    Build,
    /// `[dev-dependencies]` — the test harness only. Never in the shipped binary.
    Dev,
    /// `[workspace.dependencies]` — inherited by members that write
    /// `{ workspace = true }`.
    Inherited,
    Other,
}

impl Section {
    /// Does a crate declared here end up inside the shipped artifact (or run during
    /// its build)? `Dev` is the one that does not.
    fn shipped(self) -> bool {
        matches!(self, Section::Link | Section::Build | Section::Inherited)
    }
}

/// One dependency line, as the manifest writes it.
#[derive(Debug, Clone)]
struct Dep {
    /// The key as written; a rename (`package = "…"`) means it is *not* the crate.
    key: String,
    /// The crate's real package name.
    package: String,
    /// The `path` value, verbatim (absolute or relative to the manifest's directory).
    path: Option<String>,
    /// `{ workspace = true }`: the path, if any, comes from the root table.
    from_workspace: bool,
    section: Section,
}

/// One member's manifest, reduced to what reachability needs.
#[derive(Debug, Clone)]
struct Manifest {
    name: String,
    /// Directory relative to `rust/`, e.g. `atlas-web`.
    dir: String,
    deps: Vec<Dep>,
}

/// Which table a `[...]` header opens, plus the dependency name for the sub-table
/// form `[dependencies.atlas-net]`.
fn section_of(header: &str) -> (Section, Option<String>) {
    let parts: Vec<&str> = header.split('.').map(str::trim).collect();
    let kind = |s: &str| match s {
        "dependencies" => Some(Section::Link),
        "build-dependencies" => Some(Section::Build),
        "dev-dependencies" => Some(Section::Dev),
        _ => None,
    };
    let inherited = parts.first().copied() == Some("workspace");
    let wrap = |section: Section| {
        if inherited && section == Section::Link {
            // `[workspace.dependencies]` declares a version for members, not an
            // edge of its own; it becomes an edge only where a member inherits it.
            Section::Inherited
        } else {
            section
        }
    };
    if let Some(last) = parts.last() {
        if let Some(section) = kind(last) {
            return (wrap(section), None);
        }
        if parts.len() >= 2 {
            if let Some(section) = kind(parts[parts.len() - 2]) {
                let name = last.trim_matches(|c| c == '\'' || c == '"').to_string();
                return (wrap(section), Some(name));
            }
        }
    }
    match parts.first().copied() {
        Some("package") => (Section::Package, None),
        _ => (Section::Other, None),
    }
}

/// A quoted field inside an inline table: `path = "../x"` in
/// `{ path = "../x", features = ["a"] }`.
///
/// Scans for `name` at a word boundary, then demands `=` and a `"`, so
/// `default-features = false` never answers for `features`.
fn field(value: &str, name: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = value[from..].find(name) {
        let at = from + offset;
        let boundary = at == 0
            || !matches!(bytes[at - 1],
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-');
        let mut i = at + name.len();
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if boundary && i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                let start = i + 1;
                if let Some(end) = value[start..].find('"') {
                    return Some(value[start..start + end].to_string());
                }
            }
        }
        from = at + name.len();
    }
    None
}

/// The body of a dependency table: one `key = value` pair.
fn fill(dep: &mut Dep, key: &str, value: &str) {
    match key {
        "package" => {
            let name = value.trim().trim_matches(|c| c == '"' || c == ',');
            if !name.is_empty() && !name.contains('{') {
                dep.package = name.to_string();
            }
        }
        "path" => {
            let raw = value.trim().trim_matches(|c| c == '"' || c == ',');
            if !raw.is_empty() && !raw.contains('{') {
                dep.path = Some(raw.to_string());
            }
        }
        "workspace" => {
            if value.trim_start().starts_with("true") {
                dep.from_workspace = true;
            }
        }
        // An inline table on one line: every field lives in the one value.
        _ => {
            if let Some(path) = field(value, "path") {
                dep.path = Some(path);
            }
            if let Some(package) = field(value, "package") {
                dep.package = package;
            }
            if field(value, "workspace").is_none() && value.contains("workspace = true") {
                dep.from_workspace = true;
            }
        }
    }
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').trim_end_matches(',').to_string()
}

/// Parse one manifest. Deliberately a line reader, not a TOML dependency: this
/// crate has no dependencies, and every construct the workspace actually uses is
/// handled here — inline tables, sub-tables, renames, `workspace = true`.
fn parse_manifest(dir: &str, text: &str) -> Manifest {
    let mut name = String::new();
    let mut deps: Vec<Dep> = Vec::new();
    let mut section = Section::Other;
    // A sub-table (`[dependencies.x]`) opens a dep whose body follows on the next
    // lines; it is pushed when the next header closes it.
    let mut open: Option<Dep> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if let Some(dep) = open.take() {
                deps.push(dep);
            }
            let header = line.trim_start_matches('[').trim_end_matches(']').trim();
            let (next, sub) = section_of(header);
            section = next;
            if next.shipped() || next == Section::Dev {
                if let Some(key) = sub {
                    open = Some(Dep {
                        package: key.clone(),
                        key,
                        path: None,
                        from_workspace: false,
                        section: next,
                    });
                }
            }
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => (pair.0.trim(), pair.1.trim()),
            None => continue,
        };
        match section {
            Section::Package => {
                if key == "name" {
                    name = unquote(value);
                }
            }
            Section::Link | Section::Build | Section::Dev | Section::Inherited => {
                match open.as_mut() {
                    Some(dep) => fill(dep, key, value),
                    None => {
                        let mut dep = Dep {
                            key: key.to_string(),
                            package: key.to_string(),
                            path: None,
                            from_workspace: false,
                            section,
                        };
                        fill(&mut dep, "inline", value);
                        deps.push(dep);
                    }
                }
            }
            Section::Other => {}
        }
    }
    if let Some(dep) = open.take() {
        deps.push(dep);
    }
    Manifest {
        name,
        dir: dir.to_string(),
        deps,
    }
}

/// `base/rel`, with `.` and `..` resolved lexically (no filesystem, no symlinks).
fn normalize(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in base.split('/').chain(rel.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "reachability cannot read {}: {}\n\
             The walk needs every member's manifest; a missing one is a finding, not an empty answer.",
            path.display(),
            error
        )
    })
}

/// The workspace: its members, the directory each one lives in, and the root's
/// inherited dependency table.
struct Workspace {
    members: Vec<Manifest>,
    /// member crate name → directory relative to `rust/`.
    dirs: BTreeMap<String, String>,
    /// `[workspace.dependencies]` by key.
    inherited: BTreeMap<String, Dep>,
}

/// The `members = [ … ]` list of the root manifest.
fn read_members(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in text.lines() {
        let line = raw.trim();
        if !inside {
            if line.starts_with("members") && line.contains('[') {
                inside = true;
                // `members = ["a", "b"]` on one line is legal too.
                if let Some((_, rest)) = line.split_once('[') {
                    if rest.contains(']') {
                        for piece in rest.split(']').next().unwrap_or("").split(',') {
                            let name = piece.trim().trim_matches('"');
                            if !name.is_empty() {
                                out.push(name.to_string());
                            }
                        }
                        return out;
                    }
                }
            }
            continue;
        }
        if line.starts_with(']') {
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for piece in line.split(',') {
            let name = piece.trim().trim_matches('"');
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

impl Workspace {
    fn load(root: &Path) -> Workspace {
        let root_manifest = read(&root.join("Cargo.toml"));
        let listed = read_members(&root_manifest);
        assert!(
            listed.len() > 15,
            "the root manifest listed {} members; this walk would then be reading \
             almost nothing. Fix `read_members` before trusting a green run.",
            listed.len()
        );
        let mut members = Vec::new();
        let mut dirs = BTreeMap::new();
        for dir in &listed {
            let text = read(&root.join(dir).join("Cargo.toml"));
            let manifest = parse_manifest(dir, &text);
            assert!(
                manifest.name.starts_with("atlas-"),
                "member {} declares package name {:?}; the walk needs it to resolve \
                 path and name dependencies",
                dir,
                manifest.name
            );
            dirs.insert(manifest.name.clone(), manifest.dir.clone());
            members.push(manifest);
        }
        let root_parsed = parse_manifest("", &root_manifest);
        let inherited = root_parsed
            .deps
            .into_iter()
            .filter(|dep| dep.section == Section::Inherited)
            .map(|dep| (dep.key.clone(), dep))
            .collect();
        Workspace {
            members,
            dirs,
            inherited,
        }
    }

    fn manifest(&self, name: &str) -> &Manifest {
        self.members
            .iter()
            .find(|member| member.name == name)
            .unwrap_or_else(|| panic!("no member named {name}; the table below lists what is there"))
    }

    /// Every workspace crate `name`'s shipped code links, one edge per manifest
    /// line, transitively. `[dev-dependencies]` is left out on purpose (see the
    /// module doc).
    fn linked(&self, name: &str) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = vec![name.to_string()];
        while let Some(current) = queue.pop() {
            for dep in self.edges(&current) {
                if dep == current || !seen.insert(dep.clone()) {
                    continue;
                }
                queue.push(dep);
            }
        }
        seen
    }

    /// The member crates one member's shipped tables name.
    ///
    /// Three spellings resolve to an edge, and all three are checked because each
    /// one hides a dependency from a reader who only looks at the obvious table:
    /// `{ path = "../x" }`, `{ workspace = true }` (the path is in the root's
    /// table), and a bare name that happens to be a member (`x = "=0.1.0"`).
    fn edges(&self, name: &str) -> Vec<String> {
        let manifest = self.manifest(name);
        let mut out: BTreeSet<String> = BTreeSet::new();
        for dep in manifest.deps.iter().filter(|dep| dep.section.shipped()) {
            let mut package = dep.package.clone();
            let mut path = dep.path.clone();
            if dep.from_workspace {
                match self.inherited.get(&dep.key) {
                    Some(inherited) => {
                        package = inherited.package.clone();
                        if path.is_none() {
                            path = inherited.path.clone();
                        }
                    }
                    // An inherited name the root does not declare is a broken
                    // manifest, which cargo would refuse; say so rather than
                    // walking past it.
                    None => panic!(
                        "{} depends on `{}` with `workspace = true`, which \
                         [workspace.dependencies] does not declare",
                        name, dep.key
                    ),
                }
            }
            let mut resolved = false;
            if let Some(path) = path {
                let absolute = normalize(&manifest.dir, &path);
                for (crate_name, dir) in &self.dirs {
                    if normalize(dir, ".") == absolute {
                        out.insert(crate_name.clone());
                        resolved = true;
                    }
                }
                if !resolved {
                    panic!(
                        "{} names path dependency `{}` at `{}`, which resolves to \
                         `{}` and is not a workspace member",
                        name, dep.key, path, absolute
                    );
                }
            }
            // A version-only requirement on a member is still an edge: the lock
            // resolves it to the member, so the compiler links the member.
            if self.dirs.contains_key(&package) {
                out.insert(package);
            }
        }
        out.retain(|crate_name| crate_name != name);
        out.into_iter().collect()
    }
}

// ---------------------------------------------------------------------------
// The source scan: what a crate's shipped code can do
// ---------------------------------------------------------------------------

/// Spelled so an alias cannot hide: the type name is not what makes a bind a bind.
const LISTENS: &[&str] = &[
    "::bind(",
    "::try_bind(",
    "TcpListener::",
    "UdpSocket::",
    "UnixListener::",
    "axum::serve",
];

/// The one flag that makes a Chrome accept remote control over a socket. Built
/// with `format!` in `atlas-browser`, so the needle is the prefix, not the whole
/// flag.
const BROWSER_FLAGS: &[&str] = &["--remote-debugging-port"];

/// Starting another program. `atlas_platform::child::command(` is the workspace's
/// own spawn boundary, so a crate that only calls it still shows as a spawner.
const SPAWNS: &[&str] = &[
    "Command::new",
    "process::Command",
    "child::command(",
];

/// What a capability site *is*, because the two shipped binaries are not held to
/// the same promise. Neither may open a port or start a browser. Only the window
/// may not start a process: `atlas-setup` compiles and runs the model server it
/// provisions (`atlas-setup/src/llama.rs`), which is its whole job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A socket opened to accept connections.
    Listens,
    /// A bind whose only use is the question "is this port free?" — the listener is moved
    /// into a `bool` and dropped in the same statement, so nothing can ever connect to it.
    Probes,
    /// A browser launched with a remote-control port.
    Browser,
    /// Another program started.
    Spawns,
}

/// What one shipped code site can do.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Capability {
    kind: Kind,
    /// `atlas-web`
    crate_name: String,
    /// `atlas-web/src/boot.rs`
    path: String,
    line: usize,
    needle: &'static str,
}

impl Capability {
    fn evidence(&self) -> String {
        format!("{}:{} `{}`", self.path, self.line, self.needle)
    }
}

fn crate_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

/// Every shipped site that listens, starts a browser, or starts a program.
///
/// One hit per (file, line, needle); the wrapped matcher already ignores layout,
/// so a call split across lines is the same hit as a call on one line.
fn capabilities(files: &[SourceFile]) -> Vec<Capability> {
    let mut out: Vec<Capability> = Vec::new();
    let mut seen: BTreeSet<(String, usize, &'static str)> = BTreeSet::new();
    for (kind, needles) in [
        (Kind::Listens, LISTENS),
        (Kind::Browser, BROWSER_FLAGS),
        (Kind::Spawns, SPAWNS),
    ] {
        for needle in needles {
            for (path, first, _last, _own, _text) in find_spanned(files, needle, 2) {
                if !seen.insert((path.clone(), first, needle)) {
                    continue;
                }
                // `TcpListener::bind(…).is_ok()` is a *probe*: the value is asked a
                // question and dropped, so the socket it opened is closed before the
                // statement ends and `accept` can never be called on it. That is a
                // different capability from a listener and it is decided by the shape of
                // the statement, not by a list of files: `atlas-config::port_free` is the
                // product's one such site, and the launcher calls it while choosing a port
                // setting that has no stored value yet.
                let probes = kind == Kind::Listens
                    && (_own.contains(".is_ok()") || _own.contains(".is_err()"));
                out.push(Capability {
                    kind: if probes { Kind::Probes } else { kind },
                    crate_name: crate_of(&path).to_string(),
                    path,
                    line: first,
                    needle,
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    out
}

// ---------------------------------------------------------------------------
// The rules, as functions a plant can call too
// ---------------------------------------------------------------------------

/// Rule: the window links no workspace crate at all.
///
/// Empty is the whole claim and it is not a coincidence: `atlas-desktop`'s
/// `[dependencies]` is eframe, egui, rusqlite and serde_json, so the window cannot
/// reach a listener, a browser or a spawn boundary even if one of them is one
/// manifest line away from another crate in this tree.
fn window_rule(linked: &BTreeSet<String>) -> Vec<String> {
    linked
        .iter()
        .map(|crate_name| {
            format!(
                "atlas-desktop links workspace crate `{}`. The window must not be able \
                 to reach anything in this tree: atlas-web binds a socket, atlas-browser \
                 starts a Chrome that writes its DevTools port, atlas-ingest reaches that \
                 browser through `ChromeBrowser::global()`. Remove the dependency edge from \
                 rust/atlas-desktop/Cargo.toml. If the window needs a rule, it needs the \
                 value that rule produces, not the crate that produces it.",
                crate_name
            )
        })
        .collect()
}

/// Rule: no crate a shipped binary links may contain a listen site or a browser
/// launch. These are the two capabilities the no-port rule is about, and they
/// apply to every shipped binary — a browser is a port with a remote-control
/// protocol behind it, whether our process or its child opened it.
///
/// A process spawn is deliberately *not* in this rule: `atlas-setup` compiles and
/// runs the model server it provisions. What must not happen is setup starting a
/// *browser*, which [`Kind::Browser`] catches.
fn no_port_rule(subjects: &[(&str, BTreeSet<String>)], capabilities: &[Capability]) -> Vec<String> {
    let mut out = Vec::new();
    for (binary, linked) in subjects {
        for capability in capabilities.iter().filter(|capability| {
            // Listening and browsers only: a probe asks the kernel a question and drops
            // the socket (`Kind::Probes`), and a spawn is the installer's own job.
            matches!(capability.kind, Kind::Listens | Kind::Browser)
        }) {
            let reachable =
                capability.crate_name == *binary || linked.contains(&capability.crate_name);
            if reachable {
                out.push(format!(
                    "`{}` can reach {} — that site opens a port or starts a browser. The \
                     shipped binary must not link it: drop the dependency edge in its \
                     Cargo.toml, or move the capability behind the binary that is allowed to \
                     have it (`atlas`).",
                    binary,
                    capability.evidence()
                ));
            }
        }
    }
    out
}

/// Rule: a crate promised as "no browser, no process" contains neither.
fn quiet_rule(binary: &str, files: &[SourceFile], capabilities: &[Capability]) -> Vec<String> {
    capabilities
        .iter()
        .filter(|capability| capability.crate_name == binary)
        .map(|capability| {
            format!(
                "`{}` is promised to open no port and start no process, but its shipped \
                 code has {} ({} shipped files scanned). Remove it: the window reads the \
                 archive and POSTs to loopback, nothing else.",
                binary,
                capability.evidence(),
                files.iter().filter(|file| crate_of(&file.path) == binary).count()
            )
        })
        .collect()
}

fn report(violations: &[String]) -> String {
    violations.join("\n  * ")
}

// ---------------------------------------------------------------------------
// The live workspace
// ---------------------------------------------------------------------------

fn workspace() -> (Workspace, Vec<SourceFile>) {
    let root = rust_dir();
    let workspace = Workspace::load(&root);
    let files = shipped_code(&root);
    (workspace, files)
}

/// The shipped files, for a rule that needs the source and not the manifests.
fn claim_files() -> Vec<SourceFile> {
    shipped_code(&rust_dir())
}

/// The window: the product's face.
const WINDOW: &str = "atlas-desktop";
/// Provisioning a fresh machine. Shipped, and promised to fetch nothing and serve
/// nothing at run time.
const SETUP: &str = "atlas-setup";
/// `atlas start` / `atlas doctor`. Serves the local API and starts the reader
/// browser by design; it is listed to *contrast*, never to be constrained here.
const LAUNCHER: &str = "atlas-launcher";

#[test]
fn the_window_links_no_workspace_crate() {
    let (workspace, _) = workspace();
    let linked = workspace.linked(WINDOW);
    assert!(
        linked.is_empty(),
        "  * {}",
        report(&window_rule(&linked))
    );
    // The same claim, stated as the number: the window's reachable member set is
    // empty, and its own manifest names exactly four crates.io packages.
    // Sorted, and compared with a sorted list: the manifest's own order is a matter of
    // where a line was added (the target table below `[dependencies]`, a section a reader
    // reorders while thinking), so an order-sensitive comparison would fail for a reason
    // that is not part of the decision this test records.
    let mut direct: Vec<String> = workspace
        .manifest(WINDOW)
        .deps
        .iter()
        .map(|dep| dep.package.clone())
        .collect();
    direct.sort();
    // `wry` is the in-process system web engine, added with the reader page: it is the
    // one dependency in this list that draws somebody else's markup, and `audit_webview.rs`
    // pins the three things that keeps it from becoming a browser (one owner, the address
    // rule on both doors, devtools off and a navigation handler). It brings no HTTP client
    // and no listener; `audit_binds.rs` and `audit_downloads.rs` say so in our source.
    assert_eq!(
        direct,
        vec![
            "eframe".to_string(),
            "egui".to_string(),
            "rusqlite".to_string(),
            "serde_json".to_string(),
            "wry".to_string()
        ],
        "atlas-desktop's direct dependencies changed. That list is a reviewed decision, \
         not an accident: this repository says the \
         window reads data/atlas.db and opens no socket, so an HTTP client, a browser or \
         a listener library here is a change to the product's promise. Update this list \
         in the same commit that justifies the new dependency in docs/THREAT-MODEL.md \
         §4.14 — and never add a workspace crate (see the test above)."
    );
}

#[test]
fn the_setup_binary_links_only_manifest_and_address_crates() {
    let (workspace, _) = workspace();
    let linked = workspace.linked(SETUP);
    // Measured, and each entry has a reason a reviewer can check in one line:
    //   atlas-guard    — the URL/token gate, asked as a library call
    //   atlas-platform — the `.exe` suffix, a file's verdict, the curated child env
    //   atlas-net      — `approved_addresses_with` + `check_hop_from`: the install
    //                    download is gated by the same address rule as every fetch,
    //                    while the 6.5 GB body streams through setup's own agent
    //   atlas-lib      — pulled in by atlas-net: the JSON both speak
    // None of the four opens a listener in shipped code (atlas-net's loopback server
    // is `#[cfg(test)] mod server;` and the scanner drops it), and none starts a
    // browser. atlas-net *is* an HTTP client, which is why the setup download path
    // is pinned separately by audit_downloads.rs; the point of this rule is that the
    // installer can never *serve*.
    let expected: BTreeSet<String> = [
        "atlas-config".to_string(),
        "atlas-guard".to_string(),
        "atlas-lib".to_string(),
        "atlas-net".to_string(),
        "atlas-platform".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        linked, expected,
        "atlas-setup's reachable member set changed. It is the provisioning tool: every \
         download is an install step, with the URL printed first, and it must never serve a \
         socket. A new edge to atlas-web, atlas-browser, atlas-ingest or atlas-launcher would \
         give the installer a listener or a browser — if a new edge is genuinely needed, name \
         it here and say in the manifest comment why it cannot serve."
    );
    // `atlas-config` arrived with the installer's own paths and settings
    // (`atlas-setup/src/import.rs`), and `atlas-config` holds the product's one bind: the
    // `port_free` probe. That is a bind-and-drop, not a listener, and this asserts the
    // distinction rather than trusting the crate's name.
    let files = claim_files();
    let probes: Vec<Capability> = capabilities(&files)
        .into_iter()
        .filter(|capability| linked.contains(&capability.crate_name) || capability.crate_name == SETUP)
        .filter(|capability| matches!(capability.kind, Kind::Probes))
        .collect();
    assert!(
        probes.iter().all(|probe| probe.path == "atlas-config/src/ports.rs"),
        "an installer-reachable crate dropped a bind-and-drop probe that is not the reviewed \
         `port_free`: {:?}",
        probes
            .iter()
            .map(Capability::evidence)
            .collect::<Vec<_>>()
    );
}

#[test]
fn no_crate_that_opens_a_port_is_reachable_from_a_shipped_binary() {
    let (workspace, files) = workspace();
    let capabilities = capabilities(&files);
    assert!(
        !capabilities.is_empty(),
        "the scan found no listen site, browser flag or spawn boundary anywhere in {} \
         shipped files. That cannot be true of this tree (atlas-web binds, atlas-browser \
         launches Chrome), so the scanner is broken — fix it before believing a green run.",
        files.len()
    );
    let window = workspace.linked(WINDOW);
    let setup = workspace.linked(SETUP);
    let subjects = [(WINDOW, window), (SETUP, setup)];
    let violations = no_port_rule(&subjects, &capabilities);
    assert!(violations.is_empty(), "  * {}", report(&violations));
}

#[test]
fn the_window_itself_opens_no_port_and_starts_no_process() {
    let (_, files) = workspace();
    let capabilities = capabilities(&files);
    let violations = quiet_rule(WINDOW, &files, &capabilities);
    assert!(violations.is_empty(), "  * {}", report(&violations));
    // The one socket the window does own is stated, so it cannot spread: an
    // outbound loopback connect, in this file, to a literal 127.0.0.1 address.
    let http = files
        .iter()
        .find(|file| file.path == "atlas-desktop/src/http.rs")
        .expect("atlas-desktop/src/http.rs is the window's one socket");
    let connects = find_spanned(&[http.clone()], "TcpStream::connect", 2);
    assert_eq!(
        connects.len(),
        1,
        "the window's socket count changed: {} connect sites in atlas-desktop/src/http.rs. \
         The window opens exactly one outbound connection per model request, to a loopback \
         literal; a second one, or a listen site, is a change to docs/THREAT-MODEL.md §4.14.",
        connects.len()
    );
}

#[test]
fn the_browser_the_flag_and_the_listener_are_unreachable_from_every_binary_the_app_starts() {
    let (workspace, files) = workspace();
    let window = workspace.linked(WINDOW);
    let setup = workspace.linked(SETUP);
    let launcher = workspace.linked(LAUNCHER);

    // `atlas start` is the binary a reader runs, and it holds none of this either: it
    // *spawns* the model server, the window and the agent, and links only its own
    // configuration and store. A listener or a browser in this closure is a port opened by
    // the process itself, which is the thing docs/NO-PORTS.md §1 measures.
    for forbidden in ["atlas-browser", "atlas-web", "atlas-ingest", "atlas-net"] {
        assert!(
            !launcher.contains(forbidden),
            "atlas start links `{forbidden}` — it must spawn what it needs instead, because \
             a linked listener or browser is a port this process opens itself. Launcher \
             closure: {launcher:?}"
        );
    }

    // The positive control, moved to the crate that *does* reach the network: the agent is
    // the one process that reads pages, and it links the fetching half of this workspace.
    // Without a control, "the window reaches nothing" could just as well mean "the walker
    // reads nothing".
    //
    // The launcher used to be the control because it served the local API and started a
    // browser; it serves nothing now (it starts the model server, the window and
    // `atlas-monitor serve`, all spawned rather than linked), so its closure is four
    // workspace crates and the control had to move.
    let agent = workspace.linked("atlas-monitor");
    for expected in ["atlas-ingest", "atlas-net", "atlas-store"] {
        assert!(
            agent.contains(expected),
            "the walk from atlas-monitor does not reach `{expected}`, so this test has \
             stopped proving anything about reachability. Agent closure: {:?}",
            agent
        );
    }
    // And neither promised binary reaches them.
    for forbidden in [
        "atlas-browser",
        "atlas-web",
        "atlas-ingest",
        "atlas-launcher",
        "atlas-ai",
        "atlas-store",
    ] {
        assert!(
            !window.contains(forbidden),
            "atlas-desktop reaches `{forbidden}`"
        );
        assert!(
            !setup.contains(forbidden),
            "atlas-setup reaches `{forbidden}`"
        );
    }
    // The agent half, and the inversion this test was rewritten for. It used to assert
    // that `atlas-ingest` *did* depend on `atlas-browser`, because `ChromeBrowser::global()`
    // was the shipped browser seam and naming it kept the window's emptiness meaningful.
    // That seam is gone: `Pipeline::native` wires `NullBrowser`, because a browser can only
    // be driven through a *listening* port and the agent must not open one. So the same
    // walk now asserts the opposite, in the order a reader checks it:
    //
    // 1. the manifest: no edge from the agent, or from `atlas-ingest`, to a browser or to
    //    the retired server crate;
    // 2. the source: no line inside the agent's closure names the remote-control flag or
    //    opens a socket — the flag is a capability site even in a comment (that is
    //    `atlas_guard::source`'s rule), so this also fails if somebody explains the old
    //    design by spelling it;
    // 3. non-vacuity: the closure was walked, and the scan does still find capabilities
    //    somewhere in this tree, or a green run would only mean the scanner went blind.
    for forbidden in ["atlas-browser", "atlas-web"] {
        assert!(
            !agent.contains(forbidden),
            "atlas-monitor reaches `{forbidden}`. Agent closure: {:?}. The agent reads \
             pages over plain HTTP: a browser is a loopback listener with a remote-control \
             protocol behind it, and `atlas-web` is a listener. Remove the manifest edge \
             that added it (`atlas-ingest/Cargo.toml` carried the browser one).",
            agent
        );
        assert!(
            !workspace
                .edges("atlas-ingest")
                .iter()
                .any(|dep| dep == forbidden),
            "atlas-ingest depends on `{forbidden}` again. `Pipeline::native` wires \
             `NullBrowser`; an edge here is how a port comes back into the agent."
        );
    }
    // The one bind the agent's closure may hold is the free-port *probe*, named for the
    // same reason `audit_binds.rs` names it: `port_free()` binds a loopback literal, asks
    // the kernel whether the port is taken, and drops the listener in the same expression.
    // Nothing ever accepts on it, and the agent itself never calls it — the launcher does.
    // Any *other* bind in this closure, and any browser at all, is a finding.
    const PROBE: &str = "atlas-config/src/ports.rs";
    let capabilities = capabilities(&files);
    for capability in &capabilities {
        if !agent.contains(&capability.crate_name) {
            continue;
        }
        match capability.kind {
            Kind::Browser => panic!(
                "the agent's closure can start a remote-controlled browser: {}. A browser \
                 is a loopback listener with a remote-control protocol behind it, and the \
                 flag belongs to `atlas-browser`, which no shipped binary may link.",
                capability.evidence()
            ),
            Kind::Listens | Kind::Probes => assert_eq!(
                capability.path,
                PROBE,
                "the agent's closure opens a listening socket: {}. The only bind it may hold \
                 is `port_free`'s probe, which asks the kernel and drops the socket at once; \
                 anything else is a port this product must not open.",
                capability.evidence()
            ),
            Kind::Spawns => {}
        }
    }
    // The walk is not vacuous: the agent's closure really does contain the crate whose
    // manifest used to carry the edge, so the absence above is an absence and not a typo
    // in the crate name.
    assert!(
        agent.contains("atlas-ingest"),
        "the walk from atlas-monitor does not reach `atlas-ingest`, so the absence of a \
         browser above proves nothing. Agent closure: {:?}",
        agent
    );
    // …and the capability scan itself still sees this tree (the window's own socket, the
    // harnesses' listeners, `atlas-browser`'s flag): an empty list would make every
    // assertion above trivially true.
    assert!(
        !capabilities.is_empty(),
        "the capability scan found nothing anywhere in {} shipped files — it is blind, and \
         a green run here means nothing",
        files.len()
    );
}

// `the_devtools_flag_lives_outside_the_windows_closure` was here. It collected every
// `--remote-debugging-port`/`--headless` capability in the shipped tree and asserted that
// neither the window's nor the installer's closure could reach the crate holding it —
// `atlas-browser`, which was the only place those flags lived. That crate was deleted
// with the web face (this task), so `capabilities(&files)` now holds no browser flag and
// the rule has no subject: it can only fail on its own emptiness assertion. Its failure
// message said to delete it if the crate went, and this is that deletion. The claim it
// protected is not lost: `audit_legacy_browser.rs` scans every shipped file for the same
// flags (`launch_needles`), which is the stronger form once no crate holds one at all.

#[test]
fn the_walk_is_not_vacuous() {
    let (workspace, files) = workspace();
    assert!(
        workspace.members.len() >= 20,
        "only {} members read from the root manifest",
        workspace.members.len()
    );
    assert!(
        files.len() > 50,
        "only {} shipped files scanned",
        files.len()
    );
    // The launcher is the smallest closure in the workspace by design: it starts children
    // and supervises them, and links only what it needs to find them and read its settings —
    // config, guard, platform, store. Measured: four. A bigger number here would mean the
    // launcher had started linking the things it is supposed to spawn.
    let launcher = workspace.linked(LAUNCHER);
    assert!(
        launcher.len() >= 4,
        "atlas-launcher reaches only {} members: {:?}",
        launcher.len(),
        launcher
    );
    let path_edges: usize = workspace
        .members
        .iter()
        .flat_map(|member| member.deps.iter())
        .filter(|dep| dep.path.is_some() || dep.from_workspace)
        .count();
    assert!(
        path_edges >= 10,
        "the manifests name only {} path dependencies; the parser is not seeing the edges \
         it claims to see",
        path_edges
    );
}

/// The table the report quotes. Run with `--nocapture` to read it.
#[test]
fn the_reachability_table() {
    let (workspace, files) = workspace();
    let capabilities = capabilities(&files);
    let window = workspace.linked(WINDOW);
    let setup = workspace.linked(SETUP);
    let launcher = workspace.linked(LAUNCHER);
    let mut opener_crates: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for capability in &capabilities {
        opener_crates
            .entry(capability.crate_name.clone())
            .or_default()
            .push(capability.evidence());
    }
    let mut names: Vec<&String> = workspace.members.iter().map(|member| &member.name).collect();
    names.sort();
    eprintln!(
        "\n{:<18} {:<8} {:<7} {:<9} {}",
        "crate", "window", "setup", "atlas", "port/browser/spawn sites (shipped code)"
    );
    let mut dead = 0usize;
    for name in names {
        let reach = |set: &BTreeSet<String>| set.contains(name);
        let window_reach = name == WINDOW || reach(&window);
        let setup_reach = name == SETUP || reach(&setup);
        let atlas_reach = name == LAUNCHER || reach(&launcher);
        if !window_reach && !setup_reach && !atlas_reach {
            dead += 1;
        }
        let sites = opener_crates
            .get(name.as_str())
            .map(|list| list.join("; "))
            .unwrap_or_default();
        eprintln!(
            "{:<18} {:<8} {:<7} {:<9} {}",
            name,
            if window_reach { "yes" } else { "no" },
            if setup_reach { "yes" } else { "no" },
            if atlas_reach { "yes" } else { "no" },
            if sites.is_empty() { "—".to_string() } else { sites }
        );
    }
    eprintln!(
        "\n{} members, {} shipped files, {} capability sites, {} members reachable from \
         neither shipped binary.\nwindow closure (workspace crates): {:?}\nsetup closure: {:?}\natlas closure ({} crates): {:?}\n",
        workspace.members.len(),
        files.len(),
        capabilities.len(),
        dead,
        window,
        setup,
        launcher.len(),
        launcher
    );
    // The table is printed for the report; the rules are the tests above.
    assert!(capabilities.len() > 0);
}

// ---------------------------------------------------------------------------
// Planted regressions: the same rules, on hostile synthetic input
// ---------------------------------------------------------------------------

/// A tiny synthetic workspace, so a rule can be shown to catch what it exists to
/// catch without editing a manifest another agent is building from.
fn planted_workspace(edges: &[(&str, &[&str])]) -> Workspace {
    let mut members = Vec::new();
    let mut dirs = BTreeMap::new();
    for (name, deps) in edges {
        let mut manifest = Manifest {
            name: name.to_string(),
            dir: name.to_string(),
            deps: Vec::new(),
        };
        for dep in *deps {
            manifest.deps.push(Dep {
                key: dep.to_string(),
                package: dep.to_string(),
                path: Some(format!("../{dep}")),
                from_workspace: false,
                section: Section::Link,
            });
        }
        dirs.insert(name.to_string(), name.to_string());
        members.push(manifest);
    }
    Workspace {
        members,
        dirs,
        inherited: BTreeMap::new(),
    }
}

#[test]
fn a_planted_edge_from_the_window_to_the_browser_is_caught() {
    // The attack: one line in atlas-desktop/Cargo.toml,
    // `atlas-browser = { path = "../atlas-browser" }`. The window would then link a
    // crate that spawns Chrome and speaks CDP over the port Chrome opens.
    let planted = planted_workspace(&[
        ("atlas-desktop", &["atlas-browser"]),
        ("atlas-browser", &[]),
        ("atlas-launcher", &["atlas-browser"]),
    ]);
    let linked = planted.linked(WINDOW);
    assert!(
        linked.contains("atlas-browser"),
        "the walk did not follow the planted edge: {:?}",
        linked
    );
    let violations = window_rule(&linked);
    assert!(
        !violations.is_empty(),
        "window_rule accepted a window that links atlas-browser"
    );
    assert!(
        violations[0].contains("atlas-browser") && violations[0].contains("Remove the dependency edge"),
        "the failure message must name the crate and the fix: {}",
        violations[0]
    );
}

#[test]
fn a_planted_transitive_edge_is_caught_and_a_dev_edge_is_not() {
    // The attack moves one hop away: the window links a crate that links the
    // browser, which is what `atlas-ingest` would be. A walk that only looked at
    // direct dependencies would miss it.
    let planted = planted_workspace(&[
        ("atlas-desktop", &["atlas-ingest"]),
        ("atlas-ingest", &["atlas-browser"]),
        ("atlas-browser", &[]),
    ]);
    let linked = planted.linked(WINDOW);
    assert!(
        linked.contains("atlas-browser"),
        "the walk stopped at the first hop: {:?}",
        linked
    );
    assert!(!window_rule(&linked).is_empty());

    // And the honest exemption: a dev-dependency is not in the shipped binary, so
    // it is not a reachability finding. A test may link the browser (that is how
    // the e2e harness and atlas-browser's own tests work).
    let mut dev = planted_workspace(&[
        ("atlas-desktop", &[]),
        ("atlas-browser", &[]),
    ]);
    dev.members[0].deps.push(Dep {
        key: "atlas-browser".to_string(),
        package: "atlas-browser".to_string(),
        path: Some("../atlas-browser".to_string()),
        from_workspace: false,
        section: Section::Dev,
    });
    assert!(
        dev.linked(WINDOW).is_empty(),
        "a dev-dependency was counted as shipped: {:?}",
        dev.linked(WINDOW)
    );
}

#[test]
fn a_planted_capability_in_a_reachable_crate_is_caught() {
    // The attack is one hop past the manifests: a crate the window links grows a
    // listener of its own. The manifest rule alone would pass.
    let mut manifest = Manifest {
        name: WINDOW.to_string(),
        dir: WINDOW.to_string(),
        deps: Vec::new(),
    };
    manifest.deps.push(Dep {
        key: "atlas-store".to_string(),
        package: "atlas-store".to_string(),
        path: Some("../atlas-store".to_string()),
        from_workspace: false,
        section: Section::Link,
    });
    let workspace = Workspace {
        members: vec![
            manifest,
            Manifest {
                name: "atlas-store".to_string(),
                dir: "atlas-store".to_string(),
                deps: Vec::new(),
            },
        ],
        dirs: [
            (WINDOW.to_string(), WINDOW.to_string()),
            ("atlas-store".to_string(), "atlas-store".to_string()),
        ]
        .into_iter()
        .collect(),
        inherited: BTreeMap::new(),
    };
    let files = vec![SourceFile {
        path: "atlas-store/src/db.rs".to_string(),
        code: "fn serve() {\n    let l = std::net::TcpListener :: bind(\"127.0.0.1:0\").unwrap();\n}\n"
            .to_string(),
    }];
    let capabilities = capabilities(&files);
    // One call, two needles: `TcpListener::` and `::bind(` are the same statement
    // in `TcpListener :: bind(…)`, so this is two capability sites and one bind.
    // What matters is that the split spelling was seen at all.
    assert!(
        capabilities
            .iter()
            .any(|capability| capability.needle == "::bind("),
        "the scan missed a bind split across a `::` and a newline: {:?}",
        capabilities
    );
    assert!(
        capabilities
            .iter()
            .any(|capability| capability.needle == "TcpListener::"),
        "the scan missed the type of a bind: {:?}",
        capabilities
    );
    let linked = workspace.linked(WINDOW);
    let violations = no_port_rule(&[(WINDOW, linked)], &capabilities);
    assert!(
        !violations.is_empty(),
        "a listen site in a crate the window links was accepted"
    );
}

#[test]
fn a_planted_browser_flag_and_spawn_in_the_window_are_caught() {
    // The window itself grows the machinery, with no manifest change at all.
    let files = vec![SourceFile {
        path: "atlas-desktop/src/app.rs".to_string(),
        code: concat!(
            "fn chrome() {\n",
            "    let flag = format!(\"--remote-debugging-port={}\", 0);\n",
            "    let mut command = std::process::Command::new(\"chrome\");\n",
            "    command.arg(flag).spawn().unwrap();\n",
            "}\n"
        )
        .to_string(),
    }];
    let capabilities = capabilities(&files);
    assert!(
        capabilities
            .iter()
            .any(|capability| capability.needle == "--remote-debugging-port"),
        "the flag a `format!` builds was not seen: {:?}",
        capabilities
    );
    assert!(
        capabilities
            .iter()
            .any(|capability| capability.needle == "Command::new"),
        "a process spawn was not seen: {:?}",
        capabilities
    );
    let violations = quiet_rule(WINDOW, &files, &capabilities);
    assert_eq!(
        violations.len(),
        capabilities.len(),
        "quiet_rule reported {} of {} capabilities",
        violations.len(),
        capabilities.len()
    );
}

#[test]
fn a_planted_allowlisted_socket_in_a_quiet_crate_is_caught() {
    // Loopback is not the window's standard. `audit_binds.rs` accepts a loopback
    // bind; the window must not bind at all, even to 127.0.0.1, because a port on
    // this machine is reachable by every other local account.
    let files = vec![SourceFile {
        path: "atlas-desktop/src/http.rs".to_string(),
        code: "fn serve() {\n    let listener = std::net::TcpListener::bind(\"127.0.0.1:0\").unwrap();\n}\n"
            .to_string(),
    }];
    let capabilities = capabilities(&files);
    assert!(!capabilities.is_empty(), "a loopback bind went unseen");
    assert!(
        !quiet_rule(WINDOW, &files, &capabilities).is_empty(),
        "a loopback listener inside the window was accepted"
    );
}
