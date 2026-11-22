//! Nothing logs a page, a prompt or a token — checked on the source.
//!
//! The models, the page text and the tool arguments are untrusted *content*, and
//! content in a log file is content that outlives the request, survives a backup
//! and gets read by whoever opens `data/app.log`. The rules, and where each is
//! proved:
//!
//! | Rule | Proved by |
//! | --- | --- |
//! | nothing appends to the app log any more | [`nothing_appends_to_the_app_log`] |
//! | …and the scan sees a writer if one appears | [`a_planted_writer_of_the_app_log_is_a_finding`] |
//! | no logging framework exists to call | [`there_is_no_logging_framework`] |
//! | console output is the argv/stdout boundary, nothing else | [`console_output_stays_at_the_process_boundary`] |
//! | a file that cannot be compiled is never mistaken for the app | [`no_compilable_file_is_a_stray_copy`] |
//!
//! `main.rs` is allowed by its name, because the style rule makes it the
//! boundary; every other file that prints needs an entry in `CONSOLE` with the
//! reason its text is not content.
//!
//! # What left this file, and why
//!
//! The app log's writer was `atlas-web`: `src/applog.rs` (bounded append, one line
//! at a time), `src/logline.rs` (redaction and control-byte refusal) and the compiled
//! `state` module that wrapped it, called from `gate.rs`'s request line. Three rules
//! pinned it — one writer, one caller, no bypass — and they went with the crate when
//! the web face was deleted (this task). An exemption for a file that no longer exists
//! is a licence for whatever is written at that path next, so the rules were retired
//! rather than silenced, and replaced by the smaller claim that is still true and
//! still worth checking: the app-log path is read (permission report, status view,
//! doctor) and never opened for writing.
//!
//! The runtime half — a secret is replaced, a `\n` cannot start a second line — lived
//! with that code (`atlas-web/src/logline.rs` and `atlas-web/src/applog.rs`) and is
//! gone with it. Redaction itself is still `atlas_guard::redact`, used wherever a
//! sentence about a failure is shown.

use atlas_guard::source::{
    dead_copy, find_spanned, find_wrapped, module_name_shaped, shipped_code, SourceFile,
};
use std::path::Path;

fn workspace() -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let files = shipped_code(&root);
    assert!(files.len() > 50, "the scanner read only {} files", files.len());
    files
}

/// A file that may print, and why.
struct Allowed {
    file: &'static str,
    why: &'static str,
}

/// `println!`/`eprintln!`/`print!` are allowed at the process boundary and in the
/// console harnesses. An entry here is a review decision: text printed to a
/// terminal is read by the human who started the process, not stored.
const CONSOLE: &[Allowed] = &[
    Allowed {
        file: "atlas-ai/src/wire.rs",
        why: "the atlas-ai CLI's stdout: one JSON object, or `{\"ok\":false,…}` — the argv/stdout contract `main.rs` used to hold",
    },
    Allowed {
        file: "atlas-bench/src/main.rs",
        why: "a benchmark harness: its report is the product",
    },
    // The four `atlas-e2e` entries (report, run, cli, temp_root) and the
    // `atlas-ui/src/bin/atlas-ui-tiles.rs` entry were removed with those crates (this task).
    // `console_output_stays_at_the_process_boundary` also asserts that every entry here
    // still prints, so a leftover entry would fail rather than excuse nothing.

    Allowed {
        file: "atlas-text/src/cli.rs",
        why: "the atlas-text CLI: argv in, JSON out, like main.rs",
    },
    // `atlas-monitor serve` is the always-on agent, and its stdout *is* the contract:
    // one JSON object per action, nothing else, ever (`serve.rs`'s own module doc). What
    // that line holds is measured, not assumed: `kind`, `id`, `tool`, `ms`, `stored`,
    // `monitor`, `alerts`, and `error` — and the last one is `crate::error::scrub`ped
    // (credential-shaped material redacted by `atlas-store`, control characters folded,
    // one line, capped at `MAX_ERROR_CHARS`). No title, no body, no publisher, no query:
    // the reader's own sentence is carried in the request row, never in the printed line.
    // The three `eprintln!`s are the loop saying a queue or the monitors did not answer,
    // scrubbed the same way, written once per pass so a broken queue cannot flood.
    Allowed {
        file: "atlas-monitor/src/serve.rs",
        why: "`atlas-monitor serve`'s stdout: one JSON action line of ids, a tool name, a duration and a scrubbed error — never a title, body or query; stderr carries the same kind of scrubbed failure sentence",
    },
];

/// Every entry carries the reason it is on the list: an allowlist without a
/// reason is a comment that lies, so the reason is asserted, not just written.
#[test]
fn every_allowlist_entry_says_why() {
    for entry in CONSOLE {
        assert!(!entry.file.is_empty(), "an entry without a file");
        assert!(
            entry.why.len() > 20,
            "{}: the reason must be a sentence, not a word",
            entry.file
        );
    }
}


/// A copy, not a module: a file name a `mod` declaration could never spell
/// (`state 2.rs`, `prompts 3.rs`, `x.rs.orig`).
///
/// A file under `src/bin/` is exempt: a binary target is named by `Cargo.toml`,
/// not by `mod`, so a hyphen in its name is Cargo's (the one exception, and it is a
/// *target*, which this rule can see and a `mod` cannot).
fn is_copy(name: &str) -> bool {
    !module_name_shaped(name)
}

/// The same question for a whole path, which is what the rules ask.
fn is_copy_path(path: &str) -> bool {
    if path.contains("/src/bin/") {
        return false;
    }
    is_copy(path.rsplit('/').next().unwrap_or(""))
}

/// The file a copy is a copy *of*: `state 2.rs` → `state.rs` in the same
/// directory, by dropping a trailing ` N` or a `.orig`-style suffix.
fn sibling_of(path: &str) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", path),
    };
    let stem = name.trim_end_matches(".rs");
    let cleaned = stem
        .rsplit_once(' ')
        .map(|(head, tail)| if tail.chars().all(|ch| ch.is_ascii_digit()) { head } else { stem })
        .unwrap_or(stem);
    let cleaned = cleaned
        .split_once(".rs")
        .map(|(head, _)| head)
        .unwrap_or(cleaned);
    if dir.is_empty() {
        format!("{}.rs", cleaned)
    } else {
        format!("{}/{}.rs", dir, cleaned)
    }
}

/// True when a copy is still unreachable: no `mod`, no `#[path]`, no `include!`
/// anywhere in the shipped tree names it.
///
/// Written out here rather than reusing `atlas_guard::source::dead_copy`, because
/// this rule is what *justifies* the other rules' use of it: a shared helper on
/// both sides of that decision could agree with itself and be wrong.
fn unreachable(files: &[SourceFile], path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.trim_end_matches(".rs");
    files.iter().all(|file| {
        !file
            .hits(name)
            .into_iter()
            .chain(file.hits(stem))
            .any(|(line, text)| {
                !file.is_comment(line)
                    && (text.contains("mod ")
                        || text.contains("#[path")
                        || text.contains("include!"))
            })
    })
}

#[test]
fn a_copy_under_src_is_never_the_app() {
    // The drift rule. A `state 2.rs` beside `state.rs` is a second copy of the log
    // writer (that pair was in `atlas-web`, deleted since), and the audit that named
    // it was right to: two files claim to be the same module and only one is
    // compiled. The answer is not an allowlist entry — an entry would bless a writer
    // — and it is not "ignore any file with a space in its name", because that is a
    // hole an `include!` walks through. The answer is the two properties every copy
    // must hold:
    // 1. **Nothing names it.** No `mod`, no `#[path]`, no `include!` anywhere in
    //    the shipped tree mentions the copy, so no compiler can reach it. If
    //    someone writes `#[path = "state 2.rs"] mod state;`, this fails on the
    //    next run.
    // 2. **Its live sibling is still there.** `state 2.rs` exists beside
    //    `state.rs`; if `state.rs` ever disappears, the copy is the only content
    //    and must be put in place, not left as a stray.
    //
    // Those two facts are why the log rules may skip a copy: a file nothing
    // compiles cannot append to `data/app.log`.
    let files = workspace();
    let mut copies = Vec::new();
    let mut misses = Vec::new();
    for file in &files {
        if !is_copy_path(&file.path) {
            continue;
        }
        copies.push(file.path.clone());
        if !unreachable(&files, &file.path) {
            misses.push(format!(
                "{} — something names this copy, so it is code the scan cannot follow: delete it or \
                 give it a module name and a `mod` declaration",
                file.path
            ));
            continue;
        }
        let sibling = sibling_of(&file.path);
        if !files.iter().any(|other| other.path == sibling) {
            misses.push(format!(
                "{} — its live sibling {} is gone, so this copy is the only content: rename it into \
                 place instead of leaving both readers guessing",
                file.path, sibling
            ));
        }
    }
    assert!(
        misses.is_empty(),
        "a copy is not harmless any more:\n{}\n(all copies: {})",
        misses.join("\n"),
        copies.join(", ")
    );
    // The tree has no copies left: the 76 the sync conflict left behind (the log
    // writer's "state 2.rs", the vulnerable "views/monitors 2.rs") were deleted, so
    // the loop above is now a claim about an empty list. That is exactly the green
    // that means nothing, so the rule proves itself on a copy this test plants: the
    // scanner must see it, must find it unreachable while nothing names it, must see
    // its live sibling, and must call it *reachable* the moment a #[path] names it.
    // Without this, "no copy is reachable" would be true because nothing is looked at.
    self_test_the_copy_rule();
}

/// The copy rule, run against a planted copy. See the caller for why.
fn self_test_the_copy_rule() {
    let root = std::env::temp_dir().join(format!("atlas-copy-rule-{}", std::process::id()));
    let src = root.join("crate").join("src");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&src).expect("a scratch crate");
    std::fs::write(src.join("lib.rs"), "pub mod state;\n").expect("a crate root");
    std::fs::write(src.join("state.rs"), "pub fn write() {}\n").expect("a live module");
    std::fs::write(src.join("state 2.rs"), "pub fn write() {}\n").expect("a planted copy");

    let files = shipped_code(&root);
    let copy_path = "crate/src/state 2.rs";
    if !files.iter().any(|file| file.path == copy_path) {
        panic!(
            "the scanner did not read the planted copy; it read {:?}",
            files.iter().map(|file| &file.path).collect::<Vec<_>>()
        );
    }
    assert!(
        is_copy_path(copy_path),
        "a name no module can spell must read as a copy: {}",
        copy_path
    );
    assert!(
        unreachable(&files, copy_path),
        "nothing names the planted copy, so it must read as unreachable"
    );
    let sibling = sibling_of(copy_path);
    assert!(
        files.iter().any(|file| file.path == sibling),
        "the planted copy's live sibling is {} — the rule must find it",
        sibling
    );

    // And the moment something names it, it is code the scan cannot follow.
    std::fs::write(
        src.join("lib.rs"),
        "pub mod state;\n#[path = \"state 2.rs\"] mod shadow;\n",
    )
    .expect("a crate root that names the copy");
    let named = shipped_code(&root);
    assert!(
        !unreachable(&named, copy_path),
        "a #[path] naming the copy must make it reachable, so the rule fails on it"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// the app log: read by the permission report, written by nobody
// ---------------------------------------------------------------------------

/// The paths at which the app log may be *named*, and what may be done there.
const APP_LOG_READERS: &[&str] = &[
    // the permission report: `show(&paths.log_app)` reads a mode
    "atlas-config/src/modes.rs",
    // the status view: lists every file the app writes into `data/`
    "atlas-config/src/view.rs",
    "atlas-launcher/src/doctor.rs",
    // the doctor's checks read each data file's mode; none is opened for writing
    "atlas-launcher/src/doctor/checks.rs",
];

/// Words that turn a mention of the app-log path into a write.
const WRITE_WORDS: &[&str] = &[
    "OpenOptions",
    "append(",
    "write",
    "create(",
    "create_new",
    "set_len",
];

/// Every mention of the app-log path, and the ones that write it.
///
/// Returns `(seen, findings)`. `seen` is counted whether or not the line is allowed,
/// so a needle that matches nothing cannot pass as a clean tree.
fn app_log_writers(files: &[SourceFile]) -> (usize, Vec<String>) {
    let mut seen = 0usize;
    let mut out = Vec::new();
    // `paths.log` alone is the field; `(` `)` `,` `;` are the forms a statement can
    // put after it. A needle without one of them would also match `log_app`.
    for needle in ["paths.log_app", "paths.log)", "paths.log,", "paths.log;"] {
        for (path, line, _, text) in find_wrapped(files, needle, 2) {
            let Some(file) = files.iter().find(|f| f.path == path) else {
                continue;
            };
            if file.is_comment(line) || dead_copy(files, &path) {
                continue;
            }
            seen += 1;
            if !APP_LOG_READERS.contains(&path.as_str()) {
                out.push(format!(
                    "{}:{} — the app-log path outside a reader: {}",
                    path, line, text
                ));
                continue;
            }
            if WRITE_WORDS.iter().any(|word| text.contains(word)) {
                out.push(format!(
                    "{}:{} — a reader is writing the log: {}",
                    path, line, text
                ));
            }
        }
    }
    (seen, out)
}

#[test]
fn nothing_appends_to_the_app_log() {
    // This replaces the three rules that pinned `atlas-web`'s writer —
    // `the_app_log_has_exactly_one_writer` (applog::write_line, called only by the
    // compiled `state` module), `the_only_caller_is_the_request_line`
    // (`gate.rs::request_line`, bounded and body-free) and `the_writer_cannot_be_bypassed`
    // (`logline::prepare`). That crate was deleted with the web face (this task), and
    // nothing in the shipped tree appends to `data/app.log` any more: the window keeps
    // what it shows on screen, the launcher bounds the child logs it owns
    // (`atlas-launcher/src/childlog.rs`), and the store keeps the archive.
    //
    // What is left to pin is smaller and still worth a rule: the path is *read* — by
    // the permission report, the status view and the doctor — and never handed to
    // anything that opens a file for writing. If a crate starts logging again, this
    // scan sees the path beside a write verb, and the reviewer has to decide with the
    // old rules in front of them instead of inheriting an exemption.
    let files = workspace();
    let (seen, misses) = app_log_writers(&files);
    assert!(
        seen > 0,
        "no shipped file names the app-log path — the needle is wrong, so the claim is untested"
    );
    assert!(
        misses.is_empty(),
        "something writes the app log:\\n{}\\n\
         the writer (`atlas-web/src/applog.rs`) was deleted with the web face; a new one is a \
         decision about redaction (`atlas_guard::redact`) and bounding, not a side effect",
        misses.join("\n")
    );
    // The writer's namespace is gone from the source, not just unwired: a call to it
    // would not compile, and a comment promising it exists would be a lie.
    let leftovers: Vec<String> = files
        .iter()
        .filter(|file| !dead_copy(&files, &file.path))
        .filter(|file| file.code.contains("applog::"))
        .map(|file| file.path.clone())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a file still calls `applog::`: {:?}",
        leftovers
    );
}

#[test]
fn a_planted_writer_of_the_app_log_is_a_finding() {
    // The rule is only a rule if it sees the attack. A new writer would open the file
    // (append, create, write) at the path the app uses; a reader would not.
    let planted_writer = planted(
        "atlas-config/src/modes.rs",
        "fn log(paths: &Paths) {\n    let _ = std::fs::OpenOptions::new().append(true).open(&paths.log_app);\n}\n",
    );
    let (seen, misses) = app_log_writers(&planted_writer);
    assert_eq!(seen, 1, "the planted write was not seen");
    assert_eq!(misses.len(), 1, "a writer of the app log was accepted");

    let planted_reader = planted(
        "atlas-config/src/view.rs",
        "fn report(paths: &Paths) {\n    show(&paths.log_app);\n}\n",
    );
    let (seen, misses) = app_log_writers(&planted_reader);
    assert_eq!(seen, 1);
    assert!(misses.is_empty(), "a permission read was called a write: {:?}", misses);

    // …and the path outside the readers is a finding even with no verb of its own:
    // a handler that is handed the log path can do anything with it.
    let planted_handler = planted(
        "atlas-ingest/src/handler.rs",
        "fn f(paths: &Paths) {\n    let sink = &paths.log;\n    let _ = sink;\n}\n",
    );
    let (_, misses) = app_log_writers(&planted_handler);
    assert_eq!(misses.len(), 1, "a handler held the app-log path and was accepted");
}

#[test]
fn there_is_no_logging_framework() {
    let files = workspace();
    for needle in ["dbg!", "tracing::", "env_logger", "simplelog"] {
        let hits = find_wrapped(&files, needle, 1);
        assert!(
            hits.is_empty(),
            "{} is in the source: a logging framework can record a body nobody reviewed ({:?})",
            needle,
            hits.first()
        );
    }
    // `log::` is checked by boundary, because `applog::` and `dialog::` contain it.
    for file in &files {
        for (line, text) in file.hits("log::") {
            if file.is_comment(line) {
                continue;
            }
            let before = text.split("log::").next().unwrap_or("");
            let boundary = before
                .chars()
                .last()
                .map(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .unwrap_or(true);
            assert!(
                !boundary,
                "{}:{} uses the `log` crate: {}",
                file.path, line, text
            );
        }
    }
}

#[test]
fn console_output_stays_at_the_process_boundary() {
    let files = workspace();
    let mut misses = Vec::new();
    let mut seen = 0usize;
    for needle in ["println!", "eprintln!", "print!", "eprint!"] {
        for (path, line, _, text) in find_wrapped(&files, needle, 2) {
            let file = files.iter().find(|f| f.path == path).expect("the file");
            if file.is_comment(line) {
                continue;
            }
            // Counted whether or not it is allowed: a scan that reports zero hits
            // is a scan whose needle is wrong, and "no library prints" would then
            // be a claim about nothing.
            seen += 1;
            // A `main.rs` is the argv/stdout boundary by the style rule, so it is
            // allowed by its name: "main.rs holds no logic — argv, stdin, stdout
            // and the exit code".
            let boundary = path.rsplit('/').next() == Some("main.rs");
            if !boundary && !CONSOLE.iter().any(|entry| entry.file == path) {
                misses.push(format!("{}:{} — {}", path, line, text));
            }
        }
    }
    assert!(seen > 0, "no console output was found — the scan is blind");
    assert!(
        misses.is_empty(),
        "a library module prints:\n{}\n\
         a print is unredacted, unbounded and cannot be turned off; add the file to CONSOLE \
         in this test only if the text it prints is not content",
        misses.join("\n")
    );
    // An entry that excuses a file that no longer prints is a licence for the next
    // person who prints there.
    for entry in CONSOLE {
        let file = files.iter().find(|file| file.path == entry.file);
        let prints = file
            .map(|file| {
                ["println!", "eprintln!", "print!", "eprint!"]
                    .iter()
                    .any(|needle| !file.hits(needle).is_empty())
            })
            .unwrap_or(false);
        assert!(
            prints,
            "{}: the entry excuses nothing — that file no longer prints; remove it",
            entry.file
        );
    }
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

#[test]
fn a_space_is_not_a_module_name() {
    // The only part of the old `a_stray_copy_is_not_the_writer_but_is_still_reported`
    // that survives: that test also called `writer_findings` on a synthetic copy of
    // `atlas-web/src/state 3.rs`, and both the writer and the helper that named it were
    // retired with the crate (this task). The copy rule itself — a file no `mod` can
    // name is drift unless nothing names it and its sibling is present — is still
    // `a_copy_under_src_is_never_the_app`, with its own planted self-test.
    assert!(!module_name_shaped("state 2.rs"), "a space made a module name");
    assert!(module_name_shaped("state.rs") && module_name_shaped("map_tiles.rs"));
    assert!(!module_name_shaped("state 2.rs") && !module_name_shaped("state.rs.orig"));
}
