//! Nothing is downloaded at run time — checked on the Rust source.
//!
//! `atlas-config`'s audit (`audit_no_downloads`) greps the executed *scripts* for
//! download primitives. It does not read `.rs`, so the same rule is applied here
//! to the crate sources, which is where the app actually lives now.
//!
//! Four claims, from strongest to weakest:
//!
//! 1. **No process is ever started that could fetch something.** No
//!    `Command::new("pip")`, `("curl")`, `("brew")`… anywhere, and — the harder
//!    half — **no program name assembled from fragments**. `format!("{}{}", "cur",
//!    "l")` is a program the phrase scan cannot see, so the rule is not "we looked
//!    for the word": it is "every program a child is started with is a literal, a
//!    path, or a value that came from settings, and every file that starts one is
//!    on the list below". A new spawn site, in a new file, needs a decision in
//!    review ([`SPAWNERS`]) and a shape a human can read ([`no_program_is_built_at_run_time`]).
//! 2. **No installer phrase is a string the app could run.** `pip install`,
//!    `git clone`, `curl -O`, `releases/download`… Only `atlas-config`'s own rule
//!    table names them, and that table is a list of predicates over a text line.
//! 3. **There is one HTTP client.** `ureq` appears in `atlas-net` and nowhere
//!    else, so there is no second path to the network to forget to gate. The install
//!    tool `atlas-setup` is the one exception and it is not the app: no binary the app
//!    runs *links* it (`the_install_tool_is_not_linked_into_the_app`), every transfer
//!    it makes is a subcommand a human typed — and the one place the product starts it
//!    is named here rather than denied. `atlas-launcher/src/setup.rs` spawns
//!    `atlas-setup --install` during the boot, and only when the boot found the model
//!    file or `bin/llama-server` missing; `--no-install` provisions nothing. That spawn
//!    is a reviewed entry in [`SPAWNERS`], not an exception to this list: the earlier
//!    wording here said `atlas start` never spawns it, which stopped being true when
//!    the launcher gained the setup step, and a comment that denies a call site is how
//!    a reviewer is talked out of looking at it. Its client exists because `atlas-net`
//!    returns a capped `Page` — exactly what an app reading a news page should get —
//!    and a 6.5 GB model is not a page.
//! 4. **The scan is not fooled by shape.** A needle is matched ignoring whitespace
//!    and across one line break, and a line that starts with `*`, `-` or `>` is
//!    not a comment unless it is inside a block comment: `curl \` continued on the
//!    next line, `pip  install` with two spaces, `curl -\nO`, and
//!    `*args = Command::new("cur".to_string() + "l")` are all the same download.
//!
//! Dependencies are fetched once at build time and pinned in `Cargo.lock`. The
//! app may *tell* a human how to install something; it may never do it.

use atlas_guard::source::{dead_copy, find_spanned, find_wrapped, shipped_code, SourceFile};
use std::path::Path;

fn workspace() -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let files = shipped_code(&root);
    assert!(files.len() > 50, "the scanner read only {} files", files.len());
    files
}

/// A hit this test accepts, and why. `needle` pins the entry to one pattern, so
/// an allowlist entry cannot cover a whole file by accident.
struct Allowed {
    file: &'static str,
    needle: &'static str,
    why: &'static str,
}

/// `atlas-config` owns the download vocabulary: one predicate per rule, over a
/// lower-cased line of a script. Naming a download is the opposite of performing
/// one, and the audit is what makes the rule checkable at all.
const VOCABULARY: &[Allowed] = &[
    Allowed {
        file: "atlas-config/src/download.rs",
        needle: "",
        why: "DOWNLOAD_RULES: the predicates that *detect* a download in a script; nothing here runs a command",
    },
    Allowed {
        file: "atlas-config/src/audit.rs",
        needle: "",
        why: "the walker that applies those predicates, and the fixtures its tests use",
    },
];

/// The one place a download URL may live in this crate: it is *printed* to the
/// human when the model file is missing, and never requested. The sentence the
/// launcher prints says so, and its test asserts that sentence.
const PRINTED_HINT: &[Allowed] = &[
    Allowed {
        file: "atlas-config/src/spec.rs",
        needle: "huggingface.co/",
        why: "default_model_url(): a constant the missing-model message prints, the same hint `setup.sh` gives",
    },
    Allowed {
        file: "atlas-launcher/src/model.rs",
        needle: "huggingface.co/",
        why: "the missing-model sentence: where a human can read about the file",
    },
];

/// The install tool: the one place that fetches, builds and installs, and why it may.
///
/// atlas-setup is not the app: no crate links it, so no binary that serves, reads or
/// answers contains its client; and it acts only on a subcommand a person typed, with
/// the URL or the argv printed before anything happens (`atlas-setup check` prints what
/// it would do and exits 3 without `--yes`). What *is* true and used to be denied here:
/// the launcher spawns `atlas-setup --install` during the boot
/// (`atlas-launcher/src/setup.rs`, reviewed in [`SPAWNERS`]), when the model file or
/// `bin/llama-server` is missing and `--no-install` was not passed. Spawning is not
/// linking: the launcher decides nothing about the transfer — no URL, no size, no
/// digest — it starts the tool and waits.
///
/// The rules it relaxes are the rules an installer must relax: it names the machine's
/// own tools in error sentences (cmake, rustup), and it speaks HTTP to fetch the model
/// file.
///
/// Every entry is pinned to one file and one needle, so none of them can cover code
/// that lands beside it.
const INSTALL_TOOL: &[Allowed] = &[
    Allowed {
        file: "atlas-setup/src/model.rs",
        needle: "",
        why: "the model download: the one transfer in the project, over https, gated by atlas_guard::urlsafe on every hop, verified by size and sha256 before it is renamed into place, and only from atlas-setup model --yes",
    },
    Allowed {
        file: "atlas-setup/src/llama.rs",
        needle: "",
        why: "the cmake sentence: cmake is named in an error and never installed by ATLAS; the tool builds with the cmake the machine has, or stops and prints the one command",
    },
    Allowed {
        file: "atlas-setup/src/main.rs",
        needle: "",
        why: "the same sentences printed by the CLI: what is missing, which subcommand fixes it, and the URL or argv that would run",
    },
    Allowed {
        file: "atlas-setup/src/check.rs",
        needle: "",
        why: "the fix column of the report: every missing line says which subcommand or manual step closes it",
    },
];

/// Phrases may be *named* by the audit vocabulary or *printed* as a hint.
fn phrase_allowlist() -> Vec<Allowed> {
    let mut out: Vec<Allowed> = VOCABULARY
        .iter()
        .map(|entry| Allowed {
            file: entry.file,
            needle: entry.needle,
            why: entry.why,
        })
        .collect();
    out.extend(PRINTED_HINT.iter().map(|entry| Allowed {
        file: entry.file,
        needle: entry.needle,
        why: entry.why,
    }));
    out.extend(INSTALL_TOOL.iter().map(|entry| Allowed {
        file: entry.file,
        needle: entry.needle,
        why: entry.why,
    }));
    out
}

/// Every file allowed to start a child process, and why. Not a word list: this is
/// the review boundary for "which code may name a program at all".
///
/// An entry with an empty `needle` covers the file — every spawn shape in it was read
/// once. An entry with a `needle` covers **one call spelling**, matched against the
/// call's own text by [`explained`], so `atlas-monitor/src/import_model.rs` may use the
/// boundary helper and not `Command::new`, and a second `child::command(` beside the
/// reviewed line is still a finding. The narrow form exists because files grow: a
/// file-wide exemption written for one line is how a reviewed decision becomes
/// permission for code nobody read.
const SPAWNERS: &[Allowed] = &[
    Allowed {
        file: "atlas-platform/src/child.rs",
        needle: "",
        why: "the one spawn boundary: every child ATLAS starts goes through this module, which cleans the environment and refuses a program it cannot resolve",
    },
    Allowed {
        file: "atlas-launcher/src/child.rs",
        needle: "",
        why: "the launcher's supervisor: the program is the binary path it resolved beside itself, plus the model/Chrome argv",
    },
    Allowed {
        file: "atlas-launcher/src/llama.rs",
        needle: "",
        why: "llama-server: invocation.program is an absolute path the launcher checked, and the argv is built from settings",
    },
    Allowed {
        file: "atlas-launcher/src/window.rs",
        needle: "",
        why: "the product's own window: the program is `atlas-desktop` resolved first beside the launcher in `target/release` and then on PATH, started with the environment allowlist and the root as its working directory. It draws a native window and fetches nothing; the launcher is the only thing that starts it",
    },
    Allowed {
        file: "atlas-launcher/src/agent.rs",
        needle: "",
        why: "the always-on agent: the program is `atlas-agent`, resolved the same way as the window. It is spawned by the launcher, supervised with the model server, and its own work is one research pass at a time through atlas-ingest's tool handlers — the launcher starts it, it does not download anything",
    },
    Allowed {
        file: "atlas-search/src/rules.rs",
        needle: "",
        why: "the rule engine asking its own binary one question: the path is resolved from the crate's own executable directory, never from the archive or the web",
    },
    // The entries below were invisible to this scan until it learned to read
    // `atlas_platform::child::command(`, the boundary helper every spawn in this
    // workspace is supposed to use instead of `Command::new`. That gap is the
    // reason they exist as a block: the scan said "every spawn site is reviewed"
    // while the newest spawn sites went through the helper and were never seen.
    // Each one is still a decision, and each is a reason a reader can check.
    Allowed {
        file: "atlas-launcher/src/setup.rs",
        needle: "",
        why: "the install tool: resolved beside the launcher then on PATH, spawned once with the environment allowlist and the root as cwd; it fetches only when the boot decided the model is missing, and --no-install forbids that",
    },
    Allowed {
        file: "atlas-ingest/src/rules/spawn.rs",
        needle: "",
        why: "the rule engine asking its own binary one question (`extract`, `lookup`): the path comes from the rules config, is checked executable, and the child gets the curated environment — it is a text engine, it fetches nothing",
    },
    Allowed {
        file: "atlas-setup/src/install.rs",
        needle: "",
        why: "`cargo --version`, printed in the install log so a person can see the toolchain. It reports a version and installs nothing; the toolchain's absence is a sentence, not an install",
    },
    Allowed {
        file: "atlas-setup/src/llama.rs",
        needle: "",
        why: "`cmake --version` and the one build of `bin/llama-server` from the llama.cpp tree that ships beside the tool — inside this repository. The argv is a local source directory and a build directory; no step configures a remote and nothing is fetched",
    },
    Allowed {
        // Pinned to the call spelling on purpose: this entry must not become
        // permission for the *whole file* to start any process. `needle` is checked
        // against the call's own text (`explained` below), so a second
        // `atlas_platform::child::command(` in the same module is a finding even with
        // this entry present, while the one reviewed call is covered.
        file: "atlas-monitor/src/import_model.rs",
        needle: "atlas_platform::child::command_with",
        why: "the agent starting `atlas-setup --import-model`: the program is resolved beside the agent then on PATH, spawned once with the environment allowlist and the root as cwd, only for a settings_set row that carries import_model. The tool owns every transfer rule (a .gguf, an https link, a size, a digest); this crate passes the string the reader typed and reports the tool's own sentence back",
    },
];

/// Programs that install or fetch something. If one of these is ever handed to
/// `Command::new`, the app downloads at run time.
const INSTALLERS: &[&str] = &[
    "\"curl\"",
    "\"wget\"",
    "\"pip\"",
    "\"pip3\"",
    "\"npm\"",
    "\"npx\"",
    "\"yarn\"",
    "\"pnpm\"",
    "\"brew\"",
    "\"git\"",
    "\"powershell\"",
    "\"pwsh\"",
    "\"choco\"",
    "\"winget\"",
    "\"apt\"",
    "\"apt-get\"",
    "\"yum\"",
    "\"dnf\"",
    "\"snap\"",
];

/// Phrases that turn a string into a download. Half of them are shell syntax, so
/// a hit means someone wrote a command line into the source.
const PHRASES: &[&str] = &[
    "pip install",
    "pip3 install",
    "python -m pip",
    "npm install",
    "npm i ",
    "cargo install",
    "brew install",
    "apt-get install",
    "choco install",
    "winget install",
    "git clone",
    "curl -O",
    "curl -L",
    "curl --output",
    "wget http",
    "wget https",
    "Invoke-WebRequest",
    "Invoke-RestMethod",
    "DownloadFile",
    "urlretrieve",
    "releases/download",
    "huggingface.co/",
    "check_for_update",
    "update check",
];

/// Not `ureq`: a crate that cannot be named cannot be called.
const OTHER_CLIENTS: &[&str] = &["reqwest", "attohttpc", "surf::", "isahc", "minreq", "curl::"];

/// How a program name may never be made: a formatting macro or a concatenation on
/// the call (or on the line it continues from). `format!("{}{}", a, b)` is the
/// shape a keyword scan cannot see, so it is refused by shape instead.
const FRAGMENT_BUILDERS: &[&str] = &[
    "format!",
    "format_args!",
    "concat!",
    "String::from",
    "push_str",
    ".to_string()",
    ".to_ascii_",
    ".replace(",
];

/// Whether `allowed` explains one spawn hit: the **file**, and — when the entry pins one —
/// the **call spelling** it names, checked against the hit's own text.
///
/// `needle.is_empty()` means the entry covers the file: every shape in it was read when
/// the entry was written. A pinned entry covers exactly one spelling, which is the stronger
/// decision: `atlas-monitor/src/import_model.rs` is reviewed for
/// `atlas_platform::child::command_with(` and for nothing else, so a `Command::new` — or a
/// second `child::command(` — appearing in that module is a finding on the next run.
fn explained(allowed: &[Allowed], path: &str, text: &str) -> bool {
    allowed.iter().any(|entry| {
        entry.file == path && (entry.needle.is_empty() || text.contains(entry.needle))
    })
}

/// A hit, minus the ones `allowed` explains.
fn unexplained(needles: &[&str], allowed: &[Allowed]) -> Vec<(String, usize, String)> {
    let files = workspace();
    let mut out = Vec::new();
    for needle in needles {
        for (path, line, _, text) in find_wrapped(&files, needle, 2) {
            let explained = allowed.iter().any(|entry| {
                entry.file == path && (entry.needle.is_empty() || entry.needle == *needle)
            });
            // A copy nothing can compile is not a download: its text is in no build.
            // `audit_logs.rs` proves every copy stays unreachable.
            if explained || dead_copy(&files, &path) {
                continue;
            }
            out.push((path, line, text));
        }
    }
    out
}

fn show(misses: &[(String, usize, String)]) -> String {
    misses
        .iter()
        .map(|(path, line, text)| format!("{}:{} — {}", path, line, text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every allowlist in this file, by the name a reader will look for.
fn allowlists() -> Vec<(&'static str, &'static [Allowed])> {
    vec![
        ("VOCABULARY", VOCABULARY),
        ("PRINTED_HINT", PRINTED_HINT),
        ("INSTALL_TOOL", INSTALL_TOOL),
        ("SPAWNERS", SPAWNERS),
    ]
}

#[test]
fn every_allowlisted_file_still_exists() {
    // An entry is a name *plus* a reason for one pattern in one file. When that file is
    // deleted the entry does not become harmless: it becomes an exemption waiting for the
    // next file to take the name. Measured: `atlas-setup/src/ui.rs` was allowlisted for
    // running exactly one `cargo install wasm-bindgen-cli`, and the module was deleted
    // when the installer was restructured — so the entry sat there exempting a path that
    // nothing owned, and any future `ui.rs` would have inherited "it may run an install"
    // without a reader deciding anything.
    //
    // A missing file is therefore an error, not a stale-but-inert line: the fix is to
    // delete the entry with the file, and to write a *new* entry — with its own reason —
    // if a new file ever needs the same latitude. The count floor keeps a scan that read
    // nothing from passing.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut stale: Vec<String> = Vec::new();
    let mut seen = 0usize;
    for (name, entries) in allowlists() {
        for entry in entries {
            seen += 1;
            if !root.join(entry.file).is_file() {
                stale.push(format!("{name}: {}", entry.file));
            }
        }
    }
    assert!(
        seen >= 15,
        "the scan read only {seen} allowlist entries — it is not reading the lists it names"
    );
    assert!(
        stale.is_empty(),
        "an allowlist names a file this tree no longer has:\n{}\n\
         fix: delete the entry that names it. An exemption is a decision about a file's \
         text; when the file is gone the decision is gone with it, and leaving the name \
         behind hands that latitude to whatever file is created at the same path next.",
        stale.join("\n")
    );
}

#[test]
fn no_process_is_ever_started_with_an_installer() {
    let misses = unexplained(INSTALLERS, VOCABULARY);
    assert!(
        misses.is_empty(),
        "an installer program name in the source:\n{}",
        show(&misses)
    );
}

#[test]
fn no_installer_phrase_is_a_string_the_app_could_run() {
    let misses = unexplained(PHRASES, &phrase_allowlist());
    assert!(
        misses.is_empty(),
        "a download phrase in the source:\n{}",
        show(&misses)
    );
}

#[test]
fn every_spawn_site_is_a_reviewed_file() {
    // Rule 1, the file half: a new `Command::new` anywhere else is a fetch
    // primitive until a human says otherwise, because a program name is the only
    // thing that turns "start a process" into "download this".
    let files = workspace();
    let mut misses = Vec::new();
    let mut seen = 0usize;
    for (path, line, _, text) in find_wrapped(&files, "Command::new", 2) {
        seen += 1;
        if !explained(SPAWNERS, &path, &text) && !dead_copy(&files, &path) {
            misses.push(format!(
                "{}:{} starts a process: {}\n    add it to SPAWNERS in this test with the reason \
                 the program cannot fetch anything",
                path, line, text
            ));
        }
    }
    // `use std::process::Command;` is the import that makes the call possible: a
    // file that imports it is a spawn site too, whether or not it calls today.
    for (path, line, _, text) in find_wrapped(&files, "process::Command", 2) {
        seen += 1;
        if !explained(SPAWNERS, &path, &text) && !dead_copy(&files, &path) {
            misses.push(format!(
                "{}:{} imports the process API: {}\n    a file that can start a process is on \
                 SPAWNERS or it does not need the import",
                path, line, text
            ));
        }
    }
    // The boundary helper. `Command::new` is not how this workspace starts a child:
    // `atlas_platform::child::command(program)` is, and a needle that reads only the
    // first spelling says "every spawn site is reviewed" while the newest ones walk
    // past it. Measured before this pass existed: `atlas-launcher/src/setup.rs:368`
    // starts the install tool through the helper, and the two loops above passed
    // without ever looking at it — the scan was green because it was blind, which is
    // worse than red. The sites this pass now sees (the browser, the rule engine, the
    // installer's `cmake` and `cargo --version`) were unreviewed for the same reason.
    //
    // Both spellings of the helper are read. `child::command_with` takes an
    // environment instead of the curated allowlist, so a scan that saw `command(`
    // and not `command_with(` would have exactly the hole it was written to close,
    // one name further along.
    for helper in ["child::command(", "child::command_with("] {
        for (path, line, _, text) in find_wrapped(&files, helper, 2) {
            seen += 1;
            if !explained(SPAWNERS, &path, &text) && !dead_copy(&files, &path) {
                misses.push(format!(
                    "{}:{} starts a process through the spawn boundary: {}\n    add it to \
                     SPAWNERS in this test with the reason the program cannot fetch anything",
                    path, line, text
                ));
            }
        }
    }
    assert!(seen > 0, "no spawn site was found at all — the scan is blind");
    assert!(
        misses.is_empty(),
        "a process is started outside the reviewed list:\n{}",
        misses.join("\n")
    );
}

#[test]
fn no_program_is_built_at_run_time() {
    // Rule 1, the shape half. This is the hole a keyword list leaves: the word
    // "curl" never appears, `format!` assembles it, and the process starts.
    let files = workspace();
    let mut misses = Vec::new();
    for (path, line, _, own, text) in find_spanned(&files, "Command::new", 2) {
        // Judged on the call's own line when the call starts there: the line above
        // can hold a `format!` for an unrelated message, and a rule that always
        // reads two lines reports innocent code until somebody allows it.
        let subject = if own.contains("Command::new") {
            own.as_str()
        } else {
            text.as_str()
        };
        let built = FRAGMENT_BUILDERS
            .iter()
            .chain(["+"].iter())
            .find(|needle| subject.contains(*needle));
        if let Some(needle) = built {
            misses.push(format!(
                "{}:{} builds a program name with `{}`: {}\n    a program is a literal path or \
                 a value that came from settings; a name assembled here is a name no scan can read",
                path, line, needle, text
            ));
        }
    }
    assert!(
        misses.is_empty(),
        "a program name is assembled at run time:\n{}",
        misses.join("\n")
    );
}

/// The exception in the one-client rule is only an exception while atlas-setup is
/// not part of the app: no crate may depend on it, so no binary that serves a
/// request, reads a page or answers the model contains its client.
///
/// Measured from the manifests, which is the only place a dependency can be declared.
#[test]
fn the_install_tool_is_not_linked_into_the_app() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut linked = Vec::new();
    let mut manifests = 0usize;
    for entry in std::fs::read_dir(&root).expect("the workspace directory").flatten() {
        let manifest = entry.path().join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "atlas-setup" {
            continue;
        }
        manifests += 1;
        let text = std::fs::read_to_string(&manifest).expect("a readable manifest");
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("atlas-setup") || trimmed.contains("atlas-setup = {") {
                linked.push(format!("{}: {}", name, trimmed));
            }
        }
    }
    assert!(manifests > 10, "the scan read only {manifests} manifests — wrong root?");
    assert!(
        linked.is_empty(),
        "a crate depends on the install tool, so the app can now fetch a model from a 
         request path — that is the boundary this exception rests on:\n{}",
        linked.join("\n")
    );
}

#[test]
fn there_is_exactly_one_http_client() {
    let files = workspace();
    let mut outside = Vec::new();
    for file in &files {
        for (line, text) in file.hits("ureq") {
            // The install tool's client is the exception INSTALL_TOOL documents; it is
            // not the app's path to the network, which is what this rule protects. It
            // is held to the same gate: the URL goes through the guard.
            if file.path.starts_with("atlas-setup/") {
                assert!(
                    file.code.contains("urlsafe::check"),
                    "{} uses an HTTP client without the app's URL rule",
                    file.path
                );
            } else if !file.path.starts_with("atlas-net/") {
                outside.push(format!("{}:{} — {}", file.path, line, text));
            }
        }
    }
    assert!(
        outside.is_empty(),
        "an HTTP client outside atlas-net (a second, ungated path to the network):\n{}",
        outside.join("\n")
    );
    // And the one client is really there, so "no second path" is not "no path".
    let net = files
        .iter()
        .find(|f| f.path == "atlas-net/src/fetch.rs")
        .expect("atlas-net/src/fetch.rs");
    assert!(
        net.code.contains("ureq::Agent"),
        "the one client is not in atlas-net"
    );
}

#[test]
fn no_other_http_client_is_even_named() {
    let misses = unexplained(OTHER_CLIENTS, &[]);
    assert!(
        misses.is_empty(),
        "a second HTTP client is named in the source:\n{}",
        show(&misses)
    );
}

#[test]
fn the_scan_would_notice_a_download_if_one_appeared() {
    // The proofs above are only worth something if the scanner sees a real hit.
    let files = workspace();
    let files_with_installer = find_wrapped(&files, "\"curl\"", 2)
        .into_iter()
        .filter(|(path, _, _, _)| !VOCABULARY.iter().any(|entry| entry.file == *path))
        .count();
    assert_eq!(
        files_with_installer, 0,
        "the corpus changed: a `\"curl\"` outside the vocabulary"
    );
    // …and the vocabulary file is where the scanner does see it, which is what
    // makes the zero above meaningful rather than a broken needle.
    assert!(
        find_wrapped(&files, "\"curl\"", 2)
            .iter()
            .any(|(path, _, _, _)| path == "atlas-config/src/download.rs"),
        "the scanner found no `\"curl\"` at all — the needle is wrong"
    );
}

// ---------------------------------------------------------------------------
// planted regressions: each one fails if its rule is reverted
// ---------------------------------------------------------------------------

fn planted(path: &str, code: &str) -> Vec<SourceFile> {
    vec![SourceFile {
        path: path.to_string(),
        code: code.to_string(),
    }]
}

/// The installer and phrase scans, over a synthetic corpus.
fn scan(files: &[SourceFile], needles: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for needle in needles {
        for (path, line, _, text) in find_wrapped(files, needle, 2) {
            out.push(format!("{}:{} — {}", path, line, text));
        }
    }
    out
}

/// The spawn rules, over a synthetic corpus: which file, and what shape.
fn spawn_findings(files: &[SourceFile]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, line, _, own, text) in atlas_guard::source::find_spanned(files, "Command::new", 2) {
        if !explained(SPAWNERS, &path, &text) {
            out.push(format!("{}:{} — unreviewed spawn site: {}", path, line, text));
            continue;
        }
        let subject = if own.contains("Command::new") { own.as_str() } else { text.as_str() };
        if FRAGMENT_BUILDERS
            .iter()
            .chain(["+"].iter())
            .any(|needle| subject.contains(*needle))
        {
            out.push(format!("{}:{} — a program name assembled at run time", path, line));
        }
    }
    out
}

#[test]
fn a_fetch_primitive_built_by_format_is_a_finding() {
    // The attack the word list cannot see: no installer name is in the file.
    let files = planted(
        "atlas-ingest/src/handlers.rs",
        "fn warm() {\n    let program = format!(\"{}{}\", \"cur\", \"l\");\n    let _ = Command::new(&program).arg(\"-O\").arg(url).status();\n}\n",
    );
    let findings = spawn_findings(&files);
    assert!(!findings.is_empty(), "a built program name was accepted");
    // …and the same shape inside a reviewed file is a finding too, so being on
    // SPAWNERS is not a licence to build a name.
    let inside = planted(
        "atlas-launcher/src/child.rs",
        "fn start() {\n    let mut command = Command::new(format!(\"{}\", \"wget\"));\n    command.spawn()\n}\n",
    );
    let findings = spawn_findings(&inside);
    assert_eq!(findings.len(), 1, "a reviewed file may still not build a name");
    assert!(findings[0].contains("assembled"), "{}", findings[0]);
    // `concat!` is the same construction with a different macro.
    let concatenated = planted(
        "atlas-launcher/src/child.rs",
        "fn start() {\n    let mut command = Command::new(concat!(\"cur\", \"l\"));\n    command.spawn()\n}\n",
    );
    assert_eq!(spawn_findings(&concatenated).len(), 1);
}

#[test]
fn a_new_spawn_site_is_not_allowed_by_default() {
    let files = planted(
        "atlas-ingest/src/handlers.rs",
        "fn warm() {\n    let _ = std::process::Command::new(\"/usr/bin/curl\");\n}\n",
    );
    let findings = spawn_findings(&files);
    assert!(
        findings.iter().any(|finding| finding.contains("unreviewed spawn site")),
        "a new file started a process without a review: {:?}",
        findings
    );
}

#[test]
fn an_alias_for_the_process_api_is_a_finding() {
    // `use std::process::Command as C;` moves the word `Command::new` out of the
    // scan. The import cannot be aliased away: the file still has to import it.
    let files = planted(
        "atlas-ingest/src/handlers.rs",
        "use std::process::Command as C;\nfn warm() {\n    let _ = C::new(\"wget\");\n}\n",
    );
    let hits = find_wrapped(&files, "process::Command", 2);
    assert_eq!(hits.len(), 1, "the aliased import was not seen");
}

#[test]
fn a_spawn_through_the_boundary_helper_is_a_finding() {
    // The newest spawn site did not spell `Command::new`: it called
    // `atlas_platform::child::command(program)`, the workspace's spawn boundary, and the
    // scan matched only `Command::new` — so `every_spawn_site_is_a_reviewed_file` passed
    // while `atlas-launcher/src/setup.rs` started the install tool with no entry in
    // SPAWNERS. A rule that reads one spelling of the only spawn primitive is not a
    // rule, it is a memory of the call sites that existed when it was written.
    //
    // This test is the rule's own proof, on a planted file: the plant is in a crate and a
    // module that is not on SPAWNERS, so a hit here is exactly what the whole-tree test
    // refuses.
    let files = planted(
        "atlas-ingest/src/handlers.rs",
        "fn warm() {\n    let _ = atlas_platform::child::command(\"/usr/bin/curl\");\n}\n",
    );
    let hits: Vec<_> = ["child::command(", "child::command_with("]
        .iter()
        .flat_map(|helper| find_wrapped(&files, helper, 2))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "the boundary helper was not seen at all: {hits:?}"
    );
    assert!(
        !explained(SPAWNERS, &hits[0].0, &hits[0].3),
        "the planted file came out reviewed: {}",
        hits[0].0
    );
    // `command_with` is the same primitive with an environment argument, and it is a
    // separate name — so it needs its own hit, not an inference from the one above.
    let with_env = planted(
        "atlas-ingest/src/handlers.rs",
        "fn warm(e: &[(String, String)]) {\n    let _ = atlas_platform::child::command_with(\"/usr/bin/wget\", e);\n}\n",
    );
    assert_eq!(
        find_wrapped(&with_env, "child::command_with(", 2).len(),
        1,
        "the `command_with` spelling is invisible to the scan"
    );
}

#[test]
fn a_pinned_spawn_entry_covers_one_call_not_the_file() {
    // A review of one line must not become a review of the module. These assertions go
    // through the same predicate the scan uses, on the entry that is pinned today:
    // `atlas-monitor/src/import_model.rs` explains the boundary-helper call it was written
    // for, and does **not** explain `Command::new` or the bare `child::command(` spelling
    // in that same file — so a spawn added beside the reviewed line is a finding with
    // nobody editing a list. The unpinned entries (`needle: ""`) still cover their files,
    // which is what their reasons say they do.
    let in_file =
        |text: &str| explained(SPAWNERS, "atlas-monitor/src/import_model.rs", text);
    assert!(
        in_file(
            "let mut command = atlas_platform::child::command_with(&program, &atlas_config::spawn_env(&[]));"
        ),
        "the reviewed call is not explained: the entry's needle does not match the code it \
         was written for"
    );
    assert!(
        !in_file("let mut command = std::process::Command::new(\"curl\");"),
        "`Command::new` in import_model.rs must be a finding: the entry pins one spelling, \
         not the file"
    );
    assert!(
        !in_file("let _ = atlas_platform::child::command(\"/usr/bin/curl\");"),
        "the `command(` spelling in import_model.rs must be a finding: it is not the call \
         this entry reviewed"
    );
    assert!(
        explained(
            SPAWNERS,
            "atlas-launcher/src/setup.rs",
            "let mut command = atlas_platform::child::command(&program);"
        ),
        "a file-wide entry stopped covering its file"
    );
}

#[test]
fn a_dereference_line_cannot_hide_a_download() {
    // Before: `*` at the start of a trimmed line read as a doc-comment
    // continuation, so an assignment through a dereference was skipped.
    let files = planted(
        "atlas-ingest/src/handlers.rs",
        "fn warm() {\n    *slot = Command::new(\"curl\");\n}\n",
    );
    assert_eq!(
        scan(&files, &["\"curl\""]).len(),
        1,
        "the dereference hid the installer"
    );
}

#[test]
fn a_command_split_across_lines_is_still_the_command() {
    // Shell and Rust both let a line break into the middle of a command.
    let split_shell = planted(
        "setup.sh",
        "#!/bin/sh\ncurl \\\n  -O https://example.test/model.gguf\n",
    );
    assert!(
        !scan(&split_shell, &["curl -O"]).is_empty(),
        "a continued curl was not a download"
    );
    let two_spaces = planted("setup.sh", "#!/bin/sh\npip  install requests\n");
    assert!(
        !scan(&two_spaces, &["pip install"]).is_empty(),
        "two spaces made a different command"
    );
    let split_rust = planted(
        "atlas-launcher/src/model.rs",
        "fn hint() {\n    let s = \"git\n clone https://example.test/x.git\";\n    s\n}\n",
    );
    assert!(
        !scan(&split_rust, &["git clone"]).is_empty(),
        "a string split over two lines was not a phrase"
    );
}

#[test]
fn prose_is_not_a_download_and_code_after_a_block_comment_is() {
    let prose = planted(
        "atlas-launcher/src/model.rs",
        "// curl -O https://example.test/x\nfn hint() -> &'static str { \"\" }\n",
    );
    assert!(
        scan(&prose, &["curl -O"]).is_empty(),
        "a comment was read as a download"
    );
    let block = planted(
        "setup.sh",
        "#!/bin/sh\n/* not shell, but */ curl -O https://example.test/x\n",
    );
    assert_eq!(
        scan(&block, &["curl -O"]).len(),
        1,
        "code after a closed block comment was dropped"
    );
}

/// The web engine brings no fetch and no spawn into **our** code.
///
/// `wry` hands a URL to the system engine; our half is a builder, a bounds rectangle and a
/// navigation callback. So: no HTTP client, no download verb and no process API in
/// `atlas-desktop/src`, and — once the module lands — none in `src/webview.rs` either.
/// `TcpStream` is not on the list: the window's own loopback POST is `src/http.rs`'s and is
/// pinned by `audit_binds.rs` and `tests/no_spawn_no_socket.rs`.
#[test]
fn the_web_engine_adds_no_download_or_spawn_to_our_code() {
    const FETCH_OR_SPAWN: &[&str] = &[
        "Command::new(",
        "child::command(",
        "child::command_with(",
        "std::process",
        "ureq::",
        "reqwest::",
        "attohttpc",
        "isahc",
        "minreq",
        "curl::",
        "wget",
        "File::create",
        "fs::write(",
        "OpenOptions",
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let manifest = std::fs::read_to_string(root.join("atlas-desktop/Cargo.toml"))
        .expect("atlas-desktop/Cargo.toml");
    assert!(
        manifest.contains("wry"),
        "atlas-desktop no longer carries `wry`: this rule is about the engine's footprint \
         in our source and has to be pointed at the crate that links it"
    );
    let files = shipped_code(&root);
    let desktop: Vec<SourceFile> = files
        .iter()
        .filter(|file| file.path.starts_with("atlas-desktop/src/"))
        .cloned()
        .collect();
    assert!(
        desktop.len() > 20,
        "the scan read only {} files under atlas-desktop/src (measured 26 when this rule \
         was written) — it would pass on an empty set",
        desktop.len()
    );
    let mut hits: Vec<String> = Vec::new();
    for needle in FETCH_OR_SPAWN {
        for (path, first, _last, _own, text) in find_spanned(&desktop, needle, 2) {
            if dead_copy(&desktop, &path) {
                continue;
            }
            // `std::process::id()` is a number, not a child: the window uses it to
            // name a temporary file (`views/brief.rs`), and a rule that calls that a
            // spawn would have to be silenced the next time a real spawn appeared.
            // Only the `std::process` needle can match it, so only that one skips,
            // and only when the statement holds no `Command`.
            if *needle == "std::process" && text.contains("process::id(") && !text.contains("Command")
            {
                continue;
            }
            hits.push(format!("{path}:{first} `{needle}` — {text}"));
        }
    }
    assert!(
        hits.is_empty(),
        "the window's source gained a fetch or a spawn while the engine was added — the \
         engine renders a page in this process, it starts no program and fetches nothing \
         on this crate's behalf:\n  {}",
        hits.join("\n  ")
    );
    // Said about the engine's own module by name, because that is the file this rule
    // exists for: `test/audit_webview.rs` pins *what* it may call, and this pins what it
    // may not — a process API of any spelling.
    let webview = desktop
        .iter()
        .find(|file| file.path == "atlas-desktop/src/webview.rs")
        .expect("atlas-desktop/src/webview.rs owns the engine");
    for needle in ["Command", "process::", "child::command"] {
        assert!(
            !webview.code.contains(needle),
            "atlas-desktop/src/webview.rs holds `{needle}`: the engine is hosted in this \
             process, so a child program here is a browser this product started"
        );
    }
}
