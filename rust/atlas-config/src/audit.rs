//! The no-download audit: walk the tree, name every finding, cache the verdict.
//!
//! Decisions: which files are code (by extension, from a fixed list), which
//! directories are not ours to read (`vendor/`, `data/`, `target/`), what a
//! finding records (file, line number, the rule's name — never the line's text if
//! it carries a credential), and that a cached verdict is reused for
//! [`AUDIT_CACHE_MAX_AGE`] seconds.
//!
//! It does not: decide what a download *is* ([`crate::download`] owns the rules),
//! or fail a build — it reports, and the caller decides.

use std::path::Path;

use serde_json::{Map, Value};

use crate::download::DOWNLOAD_RULES;
use crate::walk::{descend, Walk};

/// Extensions the audit reads: code that can run.
///
/// `.rs` is on the list because the app is Rust: until it was, the verdict the
/// running app publishes at `/api/status.security.downloads` covered the setup
/// scripts and not one line of the binary they install. `rust/atlas-guard/tests/
/// audit_downloads.rs` asks the same question of the crate sources with a stricter
/// rule table; this list is the weaker net over the whole tree, and the weakest net
/// that is still worth casting is one that reads the language the app is in.
pub const AUDIT_EXTENSIONS: [&str; 7] = [".py", ".sh", ".ps1", ".cmd", ".bat", ".js", ".rs"];

/// Directories skipped by name **at the root of the walk**, because they are
/// vendored, generated or data.
///
/// Names only at the root: skipping `vendor` wherever it appears meant that a
/// directory called `vendor` anywhere in the tree was invisible, and a name is one
/// `mkdir` away. A directory deeper than the root is entered unless its whole path
/// is in [`AUDIT_SKIP_PATHS`], which is a written-down decision.
pub const AUDIT_SKIP_DIRS: [&str; 9] = [
    ".git",
    "node_modules",
    "data",
    "models",
    "vendor",
    "build",
    "dist",
    "__pycache__",
    ".vibe",
];

/// Directory names skipped wherever they appear, because a build tool writes them
/// and no human authors one: cargo puts `target/` beside every crate, which is
/// `rust/target` here and would otherwise be a walk through tens of gigabytes of
/// generated files. The list is deliberately this short — every other skip is an
/// explicit path.
pub const AUDIT_GENERATED_DIRS: [&str; 1] = ["target"];

/// Paths (relative to the root) that are skipped whole.
///
/// No `…/target` entries: [`AUDIT_GENERATED_DIRS`] already skips that name, so a
/// path entry for it would be a line that never fires — the same smell as an
/// allowlist entry that excuses nothing.
pub const AUDIT_SKIP_PATHS: [&str; 3] =
    ["tools/llama.cpp", "tools/llama-bin", "public/vendor"];

/// The one crate this audit leaves out, because it is not the app.
///
/// atlas-setup provisions a fresh machine: it fetches the pinned model file, builds
/// the model server from the vendored llama.cpp tree and installs the wasm toolchain.
/// Every one of those is a subcommand a person types, with the URL or the argv printed
/// first, and `--yes` required. No binary the app runs links it and `atlas start`
/// never spawns it (atlas-guard's `the_install_tool_is_not_linked_into_the_app`
/// measures that from the manifests), so the claim this audit makes — "nothing the app
/// executes can fetch" — is still the whole truth. The exemption is written here and
/// counted in the verdict, instead of being an accident of phrasing that a scanner
/// happened not to match.
pub const AUDIT_INSTALL_TOOL: &str = "rust/atlas-setup/";

/// How many findings — and how many of the files that were read — the report
/// carries. The counts are always exact; the lists are capped so one status page
/// cannot grow without a bound.
pub const AUDIT_LIST_MAX: usize = 40;


// ---------------------------------------------------------------------------
// the audit
// ---------------------------------------------------------------------------

/// One finding: which file, which line, which rule. Never a line of content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub rule: String,
}

impl Finding {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("file".to_string(), Value::String(self.file.clone()));
        map.insert("line".to_string(), Value::from(self.line));
        map.insert("rule".to_string(), Value::String(self.rule.clone()));
        Value::Object(map)
    }
}

/// The audit's answer: a verdict, counts, and at most [`AUDIT_LIST_MAX`]
/// findings and read files.
#[derive(Clone, Debug)]
pub struct Audit {
    pub ok: bool,
    pub findings: Vec<Finding>,
    pub finding_count: usize,
    /// How many files were opened and read.
    pub scanned: usize,
    /// How many files or directories were knowingly not read.
    pub skipped: usize,
    /// Code files exempted by a written-down decision: the install tool, and nothing
    /// else. Counted rather than skipped, so "0 findings" cannot be read as "the whole
    /// tree was judged".
    pub exempt: usize,
    /// Code files that were opened and could not be decoded as text. Also counted
    /// in `skipped`: a file the audit could not read is not a file it cleared.
    pub unread: Vec<String>,
    pub root: String,
    /// The first [`AUDIT_LIST_MAX`] files that were read, relative to the root —
    /// so "ok" can be checked by a person instead of trusted.
    pub read: Vec<String>,
}

impl Audit {
    /// True when the walk read no code file at all, so `ok` proves nothing.
    ///
    /// This is the difference between "nothing in the tree downloads" and "I
    /// looked at nothing and found nothing". A wrong `root`, a tree that holds
    /// no `.py/.sh/.ps1/.cmd/.bat/.js` file, or a skipped directory all land
    /// here, and the verdict says so instead of printing a green `ok`.
    pub fn vacuous(&self) -> bool {
        self.scanned == 0
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("ok".to_string(), Value::Bool(self.ok));
        map.insert("vacuous".to_string(), Value::Bool(self.vacuous()));
        map.insert(
            "findings".to_string(),
            Value::Array(self.findings.iter().map(Finding::to_json).collect()),
        );
        map.insert("finding_count".to_string(), Value::from(self.finding_count));
        map.insert("scanned".to_string(), Value::from(self.scanned));
        map.insert("skipped".to_string(), Value::from(self.skipped));
        map.insert("exempt_install_tool".to_string(), Value::from(self.exempt));
        map.insert(
            "unread".to_string(),
            Value::Array(
                self.unread
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
        map.insert("root".to_string(), Value::String(self.root.clone()));
        map.insert(
            "read".to_string(),
            Value::Array(
                self.read
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
        map.insert(
            "rules".to_string(),
            Value::Array(
                DOWNLOAD_RULES
                    .iter()
                    .map(|rule| Value::String(rule.name.to_string()))
                    .collect(),
            ),
        );
        Value::Object(map)
    }

    /// One line, for a boot check that must not print a file's contents.
    pub fn summary(&self) -> String {
        if self.vacuous() {
            return format!(
                "downloads: nothing was read under {} (no {} file) — the verdict proves nothing; \
                 check the root",
                self.root,
                AUDIT_EXTENSIONS.join("/")
            );
        }
        format!(
            "downloads: {} finding(s) in {} scanned file(s), {} skipped, {} exempt (the install \
             tool: not linked into the app, runs only when a person asks)",
            self.finding_count, self.scanned, self.skipped, self.exempt
        )
    }
}

/// Grep the tree for download primitives; a regression must fail loudly.
///
/// Nothing is cached here: this is a pure function of the tree, so a boot check
/// gets a fresh answer. A status page that asks repeatedly wants
/// [`audit_cached`] instead.
pub fn audit_no_downloads(root: impl AsRef<Path>) -> Audit {
    let root = root.as_ref().to_path_buf();
    let root_text = crate::paths::strip_trailing_slash(&crate::paths::show(&root));
    let mut walk = Walk::default();
    descend(&root, "", &mut walk, 0);
    let finding_count = walk.findings.len();
    let mut findings = walk.findings;
    findings.truncate(AUDIT_LIST_MAX);
    Audit {
        ok: finding_count == 0,
        findings,
        finding_count,
        scanned: walk.scanned,
        skipped: walk.skipped,
        exempt: walk.exempt,
        unread: walk.unread,
        root: root_text,
        read: walk.read,
    }
}

/// True when nothing in the tree can fetch anything — the shortcut boot checks
/// ask for.
pub fn downloads_ok(root: impl AsRef<Path>) -> bool {
    audit_cached(root, AUDIT_CACHE_MAX_AGE).ok
}

/// How long a cached verdict may be reused: the same 60 seconds the Python
/// module used, because a status page asks repeatedly and the tree changes at
/// most when an agent writes a file.
pub const AUDIT_CACHE_MAX_AGE: f64 = 60.0;

/// How many roots a cached verdict is kept for. One slot was enough for a single
/// status page and wrong for everything else: a second root (a test tree, the
/// shipped tree) evicted the first, so the next question about it re-walked —
/// and two callers reading each other's slot got each other's answer.
pub const AUDIT_CACHE_ROOTS: usize = 8;

type CacheEntry = std::collections::BTreeMap<String, (std::time::Instant, Audit)>;

static AUDIT_CACHE: std::sync::LazyLock<std::sync::Mutex<CacheEntry>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));

fn cache_lock() -> std::sync::MutexGuard<'static, CacheEntry> {
    match AUDIT_CACHE.lock() {
        Ok(guard) => guard,
        // A poisoned lock must not turn a status request into a panic.
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The audit, reused for `max_age` seconds when the same root is asked about.
///
/// A status page polls this once per request and the answer only changes when a
/// file changes, so the walk is cached per root exactly as `config._download_cache`
/// was. [`audit_no_downloads`] stays pure: a boot check that wants a fresh
/// answer calls it directly.
pub fn audit_cached(root: impl AsRef<Path>, max_age: f64) -> Audit {
    let root = crate::paths::strip_trailing_slash(&crate::paths::show(root.as_ref()));
    {
        let guard = cache_lock();
        if let Some((at, audit)) = guard.get(&root) {
            if at.elapsed().as_secs_f64() < max_age.max(0.0) {
                return audit.clone();
            }
        }
    }
    let audit = audit_no_downloads(&root);
    let mut guard = cache_lock();
    // Bounded: the oldest entry goes when the map is full, so a caller cannot
    // grow this with a loop over roots.
    if !guard.contains_key(&root) && guard.len() >= AUDIT_CACHE_ROOTS {
        let oldest = guard
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            guard.remove(&oldest);
        }
    }
    guard.insert(root, (std::time::Instant::now(), audit.clone()));
    audit
}

/// Files directly in a directory that the walk refused to enter, so `skipped`


#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::rule_hit;
    use crate::tests::*;


fn audit_fixture(files: &[(&str, &str)]) -> (TempDir, Audit) {
    let dir = TempDir::new("audit");
    for (name, body) in files {
        dir.write(name, body);
    }
    let audit = audit_no_downloads(&dir.path);
    (dir, audit)
}

#[test]
fn the_audit_finds_a_download_and_names_the_rule() {
    let (_dir, audit) = audit_fixture(&[("scripts/fetch.sh", "curl -O https://example.test/x\n")]);
    assert!(!audit.ok);
    assert_eq!(audit.finding_count, 1, "{:?}", audit.findings);
    assert_eq!(audit.findings[0].file, "scripts/fetch.sh");
    assert_eq!(audit.findings[0].line, 1);
    assert_eq!(audit.findings[0].rule, "curl-download");
    assert_eq!(audit.scanned, 1);
    assert!(audit.summary().starts_with("downloads: 1 finding(s)"));
}

#[test]
fn every_rule_fires_on_its_own_shape() {
    let cases: &[(&str, &str, &str)] = &[
        ("a.py", "pip install requests\n", "pip-install"),
        ("a2.py", "pip3 install requests\n", "pip-install"),
        ("b.sh", "git clone https://example.test/x.git\n", "git-clone"),
        ("c.py", "blob = urllib.urlretrieve(x)\n", "urlretrieve"),
        ("d.py", "data = urlopen(MODEL_URL).read()\n", "urlopen-download"),
        ("e.sh", "wget https://example.test/x\n", "wget"),
        ("f.sh", "curl --output model.gguf https://example.test/m\n", "curl-download"),
        (
            "g.ps1",
            "Invoke-WebRequest -Uri https://example.test/x -OutFile x\n",
            "powershell-download",
        ),
        (
            "h.py",
            "x = urllib.request.urlopen('https://huggingface.co/a/b')\n",
            "url-fetch",
        ),
        (
            "i.js",
            "const u = 'https://api.github.com/v1/curl';\n",
            "host-fetch",
        ),
        ("j.sh", "npx playwright install chromium\n", "browser-download"),
    ];
    for (name, body, expected) in cases {
        let (_dir, audit) = audit_fixture(&[(name, body)]);
        assert!(!audit.ok, "{} was not flagged", body);
        assert_eq!(
            audit.findings[0].rule, *expected,
            "{} -> {:?}",
            body, audit.findings
        );
    }
}

#[test]
fn the_tunnel_rule_is_reachable_on_a_line_without_a_loopback_address() {
    let (_dir, audit) = audit_fixture(&[("t.sh", "cloudflared tunnel run\n")]);
    assert!(!audit.ok);
    assert_eq!(audit.findings[0].rule, "tunnel");
    // A local target is an allowed line: that is the app talking to itself.
    let (_dir, audit) = audit_fixture(&[("t.sh", "ngrok http 127.0.0.1:8787\n")]);
    assert!(audit.ok);
}

#[test]
fn the_audit_never_reports_its_own_rules_or_a_printed_hint() {
    let cases: &[(&str, &str)] = &[
        ("a.py", "# pip install requests\n"),
        ("b.py", "print(\"run pip install requests\")\n"),
        (
            "c.py",
            "MODEL_URL = \"https://huggingface.co/a/b/resolve/main/c.gguf\"\n",
        ),
        (
            "d.py",
            "GEO_MAP_URL = ('https://raw.githubusercontent.com/x/y.geojson')\n",
        ),
        (
            "e.py",
            "    (\"pip-install\", re.compile(r\"\\bpip3?\\b[^|;&\\n]*\\binstall\\b\", re.I)),\n",
        ),
        ("g.sh", "  # curl -O https://example.test/x\n"),
        ("h.sh", "echo \"install it with: pip install requests\"\n"),
        ("i.py", "log.warning('wget is not used here')\n"),
        ("j.py", "// wget https://example.test/x\n"),
        ("k.py", "url = \"http://127.0.0.1:8787/api/status\"\n"),
        ("l.py", "note = 'see the docs for the manual step'\n"),
    ];
    for (name, body) in cases {
        let (_dir, audit) = audit_fixture(&[(name, body)]);
        assert!(audit.ok, "{} was flagged: {:?}", body, audit.findings);
    }
}

#[test]
fn a_printed_hint_is_still_a_hint_but_a_redirect_is_not() {
    // The excuse "it only talks about a download" used to cover any line with the
    // word `log`, `info` or `note` in it — including the line that actually
    // fetched, with its output sent to a log. A hint is text a person reads and
    // never ends in `> file`.
    let (_dir, audit) = audit_fixture(&[
        ("a.sh", "curl -O https://example.test/model.gguf >> data/fetch.log\n"),
        ("b.sh", "wget https://example.test/x 2> err.txt\n"),
    ]);
    assert!(!audit.ok, "a redirected fetch was excused as a hint");
    assert_eq!(audit.finding_count, 2, "{:?}", audit.findings);
    // A `>` inside a quoted sentence is text, and a `->` is an arrow, not a
    // redirect: both keep the hint a hint.
    let (_dir, audit) = audit_fixture(&[
        ("c.sh", "echo \"install with: pip install requests > /dev/null\"\n"),
        ("d.py", "note = 'run pip install x and you are done'\n"),
        (
            "e.py",
            "def f() -> str:\n    pass  # note: pip install x is the manual step\n",
        ),
    ]);
    assert!(audit.ok, "{:?}", audit.findings);
}

#[test]
fn the_audit_skips_vendored_and_data_trees() {
    let dir = TempDir::new("audit-skip");
    dir.write("node_modules/index.js", "pip install requests\n");
    dir.write("data/cache/x.py", "pip install requests\n");
    dir.write("vendor/lib.py", "pip install requests\n");
    dir.write("tools/llama.cpp/convert.py", "pip install requests\n");
    dir.write("keep.py", "pip install requests\n");
    let audit = audit_no_downloads(&dir.path);
    assert_eq!(audit.finding_count, 1, "{:?}", audit.findings);
    assert_eq!(audit.findings[0].file, "keep.py");
    assert!(audit.skipped >= 1);
}

#[test]
fn the_audit_reads_only_executed_code() {
    let dir = TempDir::new("audit-ext");
    dir.write("notes.md", "pip install requests\n");
    dir.write("config.json", "{\"cmd\": \"pip install requests\"}\n");
    dir.write("readme.txt", "pip install requests\n");
    let audit = audit_no_downloads(&dir.path);
    assert!(audit.ok);
    assert_eq!(audit.scanned, 0);
}

#[test]
fn this_crate_downloads_nothing() {
    // The regression guard: the crate we are looking at must stay clean.
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let audit = audit_no_downloads(here);
    assert!(audit.ok, "atlas-config itself: {:?}", audit.findings);
    assert!(audit.to_json().get("rules").is_some());
    // Twelve since `program-name`: the argv spelling of a fetch (`Command::new("curl")`)
    // is the same download as the shell line the shell rules read.
    assert_eq!(DOWNLOAD_RULES.len(), 12);
    assert_eq!(DOWNLOAD_RULES[11].name, "program-name");
    assert_eq!(DOWNLOAD_RULES[0].name, "pip-install");
    // `.rs` is the seventh: the app is Rust, and the verdict this crate publishes
    // at boot used to cover the setup scripts and not one line of the binary they
    // install. `public/vendor` is skipped by path — a name skip for `vendor` was a
    // place to hide a fetch one `mkdir` away.
    assert_eq!(AUDIT_EXTENSIONS.len(), 7);
    assert!(AUDIT_EXTENSIONS.contains(&".rs"));
    assert_eq!(AUDIT_SKIP_PATHS.len(), 3);
    assert!(AUDIT_GENERATED_DIRS.contains(&"target"));
    assert_eq!(
        rule_hit("curl -O https://example.test/x"),
        Some("curl-download")
    );
    assert_eq!(rule_hit("this line does nothing"), None);
    assert!(downloads_ok(here));

}
// ---------------------------------------------------------------------------
// planted regressions: each one fails if the walk's rule is reverted
// ---------------------------------------------------------------------------

#[test]
fn a_download_below_the_old_line_cap_is_still_found() {
    // The walk used to read `text.lines().take(6000)`: 6000 lines of padding and
    // then a `curl -O` was a file the audit reported as scanned and clean.
    let dir = TempDir::new("audit-long");
    let mut body = String::from("#!/bin/sh\n");
    body.push_str(&"# padding\n".repeat(6000));
    body.push_str("curl -O https://example.test/model.gguf\n");
    dir.write("scripts/long.sh", &body);
    let audit = audit_no_downloads(&dir.path);
    assert!(!audit.ok, "a download past line 6000 was invisible");
    assert_eq!(audit.findings[0].line, 6002, "{:?}", audit.findings);
}

#[test]
fn a_directory_named_vendor_below_the_root_is_walked() {
    // The name skip used to apply at any depth, so `scripts/vendor/` was a place
    // to put a download and be skipped: `AUDIT_SKIP_DIRS` is now root-only and
    // anything deeper is an explicit `AUDIT_SKIP_PATHS` entry.
    let dir = TempDir::new("audit-nested-vendor");
    dir.write("vendor/lib.py", "pip install requests\n");
    dir.write("scripts/vendor/fetch.sh", "curl -O https://example.test/x\n");
    dir.write("tools/llama.cpp/convert.py", "pip install requests\n");
    let audit = audit_no_downloads(&dir.path);
    assert_eq!(audit.finding_count, 1, "{:?}", audit.findings);
    assert_eq!(audit.findings[0].file, "scripts/vendor/fetch.sh");
    // …and the root-level vendor is still skipped, because that one is ours to
    // declare: the decision is written down, not guessed from a name.
    assert!(audit.skipped >= 2, "the root vendor was not skipped: {audit:?}");
}

#[test]
fn a_code_file_that_cannot_be_read_is_named_not_cleared() {
    // A `.sh` saved as UTF-16 (or with one invalid byte) fails `read_to_string`.
    // It used to `continue` without touching a counter, so the file was neither
    // scanned nor skipped: the verdict said "ok" about a file nobody read.
    let dir = TempDir::new("audit-unreadable");
    dir.write("scripts/ok.sh", "echo hello\n");
    let bad = dir.path.join("scripts/bad.sh");
    std::fs::write(&bad, [0xff, 0xfe, b'p', 0x00, b'i', 0x00, b'p', 0x00])
        .expect("write the undecodable fixture");
    let audit = audit_no_downloads(&dir.path);
    assert_eq!(audit.scanned, 1, "the readable file was not read");
    assert_eq!(audit.skipped, 1, "{audit:?}");
    assert!(
        audit.unread.iter().any(|name| name == "scripts/bad.sh"),
        "the unreadable file was not named: {:?}",
        audit.unread
    );
    // It is published, so a person can see what the verdict did not cover.
    let json = audit.to_json();
    assert!(json.get("unread").is_some(), "{}", json);
}

#[test]
fn a_rust_table_is_data_and_a_rust_call_is_a_download() {
    // The pair that lets `.rs` be scanned at all: the rule table names a download
    // as a string, and a string cannot fetch; a call has a `(` and is the thing
    // worth reading.
    let dir = TempDir::new("audit-rust-shapes");
    dir.write(
        "atlas-config/src/table.rs",
        "const NAMES: [&str; 2] = [\"invoke-webrequest\", \"downloadfile\"];\nDownloadRule { name: \"pip-install\", check: check_pip },\n",
    );
    let audit = audit_no_downloads(&dir.path);
    assert!(
        audit.ok,
        "the audit reported its own vocabulary as a download: {:?}",
        audit.findings
    );
    // The same names in a call are a finding, whether the line is one call or a
    // call split over two lines.
    dir.write(
        "atlas-web/src/probe.rs",
        "fn warm() {\n    let _ = std::process::Command::new(\"curl\").arg(\"-O\").spawn();\n}\n",
    );
    let audit = audit_no_downloads(&dir.path);
    assert!(!audit.ok, "a fetch written as a call was accepted as data");
}

#[test]
fn rust_test_code_is_test_data_and_shipped_code_is_not() {
    // `#[cfg(test)]` blocks are blanked by the same function the crate audits use,
    // and `tests/` directories are skipped whole. A fixture that says `pip
    // install` is a fixture; the shipped line below it is not.
    let dir = TempDir::new("audit-rust-test-code");
    dir.write(
        "atlas-web/src/shapes.rs",
        "pub fn clean() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn fixture() {\n        let _ = run(\"pip install requests\");\n    }\n}\n",
    );
    let audit = audit_no_downloads(&dir.path);
    assert!(audit.ok, "{:?}", audit.findings);
    assert_eq!(audit.scanned, 1, "the shipped file was not read");
    dir.write(
        "atlas-web/tests/harness.rs",
        "fn f() { let _ = run(\"pip install requests\"); }\n",
    );
    let audit = audit_no_downloads(&dir.path);
    assert!(
        audit.ok,
        "a test directory was read as shipped code: {:?}",
        audit.findings
    );
    // …and shipped code is: the same call outside a cfg(test) block is a finding.
    dir.write(
        "atlas-web/src/shapes.rs",
        "pub fn warm() { let _ = run(\"pip install requests\"); }\n",
    );
    let audit = audit_no_downloads(&dir.path);
    assert!(!audit.ok, "a shipped fetch was excused");
}

#[test]
fn a_missing_root_is_empty_and_not_a_panic() {
    let audit = audit_no_downloads("/nonexistent-atlas-config-root");
    assert!(audit.ok);
    assert_eq!(audit.scanned, 0);
    assert_eq!(audit.finding_count, 0);
    // "ok" because nothing was read is not the same claim as "nothing downloads"
    // and the verdict has to say which one it is making.
    assert!(audit.vacuous());
    assert!(audit.summary().contains("proves nothing"), "{}", audit.summary());
    assert_eq!(audit.to_json().get("vacuous"), Some(&Value::Bool(true)));
}

/// The probe the brief asks for: a rule table that is wired to a walk, proven by
/// planting one download per scanned extension — the six extensions the shipped
/// tree actually uses (`setup.sh`, `run.ps1`, `public/ui/loader.js`, …).
#[test]
fn a_planted_download_is_caught_through_the_walk_in_every_scanned_extension() {
    let cases: [(&str, &str, &str); 7] = [
        ("scripts/a.py", "pip install requests\n", "pip-install"),
        ("scripts/b.sh", "git clone https://example.test/x.git\n", "git-clone"),
        ("scripts/c.ps1", "Invoke-WebRequest -Uri https://example.test/x -OutFile x\n", "powershell-download"),
        ("scripts/d.cmd", "curl -O https://example.test/x.exe\n", "curl-download"),
        ("scripts/e.bat", "wget https://example.test/x.zip\n", "wget"),
        ("public/ui/f.js", "npx playwright install chromium\n", "browser-download"),
        // The extension the app is written in: one call, on one line, one finding.
        (
            "rust/atlas-web/src/probe.rs",
            "fn warm() { let _ = run(\"pip install requests\"); }\n",
            "pip-install",
        ),
    ];
    assert_eq!(cases.len(), AUDIT_EXTENSIONS.len(), "one case per scanned extension");
    let mut files: Vec<(&str, &str)> = cases.iter().map(|(name, body, _)| (*name, *body)).collect();
    files.push(("notes.md", "pip install requests\n"));
    let (_dir, audit) = audit_fixture(&files);
    assert!(!audit.ok);
    assert!(!audit.vacuous());
    assert_eq!(audit.finding_count, cases.len(), "{:?}", audit.findings);
    for (name, body, rule) in cases {
        assert!(
            audit
                .findings
                .iter()
                .any(|finding| finding.file == name && finding.rule == rule),
            "{} ({}) was not caught: {:?}",
            name,
            body,
            audit.findings
        );
    }
    // The list of read files is what a person checks instead of trusting "ok".
    assert_eq!(audit.read.len(), cases.len(), "{:?}", audit.read);
    assert!(audit.read.contains(&"scripts/a.py".to_string()));
}

#[test]
fn the_shipped_tree_is_read_rather_than_assumed() {
    // The audit that guards the release is `atlas-config audit` on the project
    // root. It has to read the tree it is given: `scanned == 0` on the shipped
    // root would mean the whole no-download claim is about an empty directory.
    // The count is measured, not asserted from a list of names, and the files it
    // read are published so a person can see what was covered.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let audit = audit_no_downloads(&root);
    assert!(
        audit.scanned > 0,
        "the audit read nothing under {}: {:?}",
        root.display(),
        audit.read
    );
    assert!(!audit.vacuous());
    assert!(audit.read.len() == audit.scanned.min(AUDIT_LIST_MAX));
    assert!(
        audit.read.iter().any(|name| name.ends_with("run.sh") || name.ends_with("setup.sh")),
        "the shipped scripts were not read: {:?}",
        audit.read
    );
    // If this fails, a shipped file fetches something: the finding names it.
    assert!(audit.ok, "the shipped tree downloads something: {:?}", audit.findings);
}

#[test]
fn two_roots_keep_their_own_cached_verdict() {
    // One global slot made two callers answer for each other: the second root
    // evicted the first, so a status page and a boot check thrashed each other's
    // verdicts (and a test that changed its tree saw the other test's answer).
    let dirty = TempDir::new("audit-cache-dirty");
    dirty.write("a.py", "pip install requests\n");
    let clean = TempDir::new("audit-cache-clean");
    clean.write("a.py", "pass\n");
    assert!(!audit_cached(&dirty.path, 60.0).ok);
    assert!(audit_cached(&clean.path, 60.0).ok);
    // The dirty root is still cached as dirty, and the clean one as clean.
    assert_eq!(audit_cached(&dirty.path, 60.0).finding_count, 1);
    assert_eq!(audit_cached(&clean.path, 60.0).finding_count, 0);
    assert_eq!(AUDIT_CACHE_ROOTS, 8);
    assert!(AUDIT_CACHE_ROOTS >= 2);
}


#[test]
fn the_cached_audit_reuses_the_answer_and_the_pure_one_does_not() {
    let dir = TempDir::new("audit-cache");
    dir.write("a.py", "pip install requests\n");
    let first = audit_cached(&dir.path, 60.0);
    assert!(!first.ok);
    // The tree changes; the cached verdict does not, until the age is zero.
    dir.write("a.py", "pass\n");
    let cached = audit_cached(&dir.path, 60.0);
    assert_eq!(cached.finding_count, 1, "the cache did not reuse the verdict");
    let fresh = audit_cached(&dir.path, 0.0);
    assert!(fresh.ok, "max_age 0 must re-walk");
    assert!(!audit_no_downloads(&dir.path).ok == false);
    assert_eq!(AUDIT_CACHE_MAX_AGE, 60.0);
    assert!(downloads_ok(&dir.path));
}
}
