//! The window's two secrecy rules, as assertions: the queue it writes, and its mouth.
//!
//! ## 1. What the request queue does with a token
//!
//! `data/requests.db` is the window's **only** writable file (`src/requests.rs:1`, pinned by
//! `tests/disclosure.rs`). A row's payload is built by a view from the reader's own words —
//! a topic, a question, a monitor's reason — and `requests::check` (`src/requests.rs`) used to
//! validate kind, size and JSON shape, not content, so a credential pasted into a question
//! was written to the file and read by the supervised agent.
//!
//! It is refused now, and **refused rather than rewritten**: a repaired payload would send the
//! agent a question the reader did not write, and `[redacted]` inside a topic name is a topic
//! nobody asked for. The sentence is `CREDENTIAL_REFUSAL`, word for word the one
//! `atlas_store::requests` uses, so a reader who meets both doors sees one rule.
//!
//! **What this door can decide, and what it cannot** — the honest boundary. The window links
//! no workspace crate (`tests/fresh_machine.rs` pins its four dependencies), so it cannot call
//! `atlas_guard::redact`. It therefore refuses what a *shape* can decide alone: a run of 40+
//! `[A-Za-z0-9_-]` bytes mixing case and carrying a digit (`requests::carries_a_token`). The
//! *named* half — `api_key=…`, `sk-live-…`, a JWT — is `atlas-guard`'s vocabulary and is
//! refused at the agent's door of the same table (`atlas-store/src/requests.rs`,
//! `CREDENTIAL_REFUSAL`). The test below states that gap rather than hiding it: making the
//! window catch named credentials means letting it link `atlas-guard`, which is a manifest
//! decision for the crate that owns that assertion.
//!
//! ## 2. The window never prints an environment variable
//!
//! The one environment value the shipped window reads is `ATLAS_MODEL_URL`
//! (`src/app.rs:122`), and `src/app.rs:134` says why the status row shows fixed text instead
//! of it: a configuration value can be a credential (`http://alice:secret@127.0.0.1:8080`).
//! The scan below is the structural half of that sentence: no shipped line in `src/` prints
//! an environment value, and the reads are exactly the ones with a reason.

use std::path::{Path, PathBuf};

/// A scratch root that does not exist yet, unique to this process.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-desktop-secret-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

/// An 80-character base64url blob: the shape a bearer token has once it is pasted somewhere
/// the app does not know the name of.
const OPAQUE: &str =
    "Zm9vYmFyYmF6cXV1eDEyMzQ1Njc4OTBhYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0NTY3ODlh";

/// A 40-byte run that mixes case and carries digits — the shortest token this rule sees.
const SHAPE_TOKEN: &str = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq";

/// Does the file's bytes contain `needle`? Reading the file itself, not the row, because the
/// claim is about what is on disk.
fn file_holds(path: &Path, needle: &str) -> bool {
    let bytes = std::fs::read(path).unwrap_or_default();
    let needle = needle.as_bytes();
    bytes
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_payload_that_carries_a_credential_never_reaches_the_queue_file() {
    // This test was `#[ignore]`d while the door accepted everything; it runs now. `Ok(1)` was
    // the measurement that made it a defect.
    let root = scratch("queue-refused");
    let queue = root.join("data").join("requests.db");
    for payload in [
        format!("{{\"query\":\"look up {SHAPE_TOKEN}\"}}"),
        format!("{{\"query\":\"look up {OPAQUE}\"}}"),
    ] {
        let outcome = atlas_desktop::requests::push_to(&queue, "research", &payload);
        assert!(
            outcome.is_err(),
            "a payload carrying a token was queued: {:?}",
            outcome
        );
        let said = outcome.err().unwrap_or_default();
        assert_eq!(
            said,
            atlas_desktop::requests::CREDENTIAL_REFUSAL,
            "the page must show one sentence, and it must be the shared one"
        );
        assert!(
            !said.contains(SHAPE_TOKEN) && !said.contains(OPAQUE),
            "the refusal repeated the credential: {said}"
        );
        assert!(
            !file_holds(&queue, SHAPE_TOKEN) && !file_holds(&queue, OPAQUE),
            "the credential reached data/requests.db"
        );
    }
    // An ordinary request still queues, word for word — the refusal must not become a filter
    // on the reader's words.
    let ordinary = "{\"query\":\"what is happening with chips?\"}";
    atlas_desktop::requests::push_to(&queue, "research", ordinary).expect("an ordinary request");
    assert!(file_holds(&queue, "what is happening with chips?"));
    let citation = "{\"query\":\"see https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/\"}";
    atlas_desktop::requests::push_to(&queue, "research", citation)
        .expect("a citation is not a credential");
    assert!(file_holds(&queue, "tsmc-halts-china-chip-shipments-2026-11-21"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "open gap: this door cannot see a *named* credential (api_key=, sk-live-…), because \
            it cannot link atlas-guard; un-ignore when it can (see the report)"]
fn a_named_credential_payload_is_refused_by_this_door_too() {
    // The gap, stated rather than hidden, and stated as the test that must pass when it is
    // closed. `carries_a_token` is a shape rule; `api_key=sk-live-abcdefghijklmnop` has no
    // 40-byte run, and telling `api_key=` from `q=` needs a vocabulary — `atlas-guard`'s,
    // which this crate cannot reach: its manifest is pinned to four dependencies by
    // `tests/fresh_machine.rs::the_window_links_no_http_client_and_no_downloader` (another
    // owner's assertion), and `src/requests.rs`'s header states that the window links no
    // workspace crate at all. The exact fix is a decision for that file's owner: allow
    // `atlas-guard` (a leaf crate with no dependencies and no network code) in the window's
    // manifest, then `check` calls `atlas_guard::redact::redact(payload)` exactly as the
    // agent's door does and this test passes unmodified.
    //
    // Until then the payload is refused at the *other* door of the same table:
    // `atlas-store/tests/config_value_secrets.rs::the_queues_other_door_refuses_a_credential_and_writes_no_row`
    // proves the agent's door refuses it, and the row the window writes here would be read by
    // that same agent — which is why this is a gap and not a hole in the archive.
    let root = scratch("queue-named");
    let queue = root.join("data").join("requests.db");
    let payload =
        "{\"query\":\"look up https://api.example/v1?q=x&api_key=sk-live-abcdefghijklmnop\"}";
    let outcome = atlas_desktop::requests::push_to(&queue, "research", payload);
    assert!(
        outcome.is_err(),
        "a payload carrying a named credential was queued: {:?}",
        outcome
    );
    assert!(!file_holds(&queue, "sk-live-abcdefghijklmnop"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_two_doors_use_one_sentence() {
    // The window repeats `atlas_store::requests::CREDENTIAL_REFUSAL` instead of importing it
    // (the crate links no workspace crate), and a copy is only honest if it is checked. The
    // store's source is read from the checkout — the same technique
    // `requests::tests::the_queue_table_and_index_are_the_stores_word_for_word` uses for the
    // schema. If the store's sentence changes, this fails and the window's copy is updated.
    let store = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate directory")
        .join("atlas-store")
        .join("src")
        .join("requests.rs");
    let source = std::fs::read_to_string(&store).expect("the store's requests.rs");
    for part in [
        "this request carries a credential; ATLAS will not store it",
        "remove it from your",
        "question and try again",
    ] {
        assert!(
            source.contains(part),
            "the store's refusal no longer contains {part:?} — update the window's copy"
        );
        assert!(
            atlas_desktop::requests::CREDENTIAL_REFUSAL.contains(part),
            "the window's sentence lost {part:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. the window's mouth
// ---------------------------------------------------------------------------

/// Every `.rs` file under `atlas-desktop/src`, with its source.
fn shipped() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // A test module is not shipped: it may print whatever it measured. Two of them
            // live beside the code (`src/data/tests.rs`, declared `#[cfg(test)] mod tests;`
            // at `src/data.rs:976`, and an orphan `src/tests.rs` that no `mod` declares) and
            // a third is an inline `#[cfg(test)] mod tests { … }`, which
            // [`without_test_modules`] strips. The orphan is named in the report: a file that
            // is never compiled is a test somebody believes is running.
            if name == "tests.rs" {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((name.to_string(), source));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    walk(&root, &mut found);
    found.sort();
    found
}

/// The source with `#[cfg(test)] mod tests { … }` removed, by brace counting.
///
/// Coarse on purpose: the scan below only needs to stop looking at a module that exists to
/// print measurements, and a false "this is test code" would weaken the scan — so the rule is
/// the attribute, not a heuristic about `println!`.
///
/// The braces inside a string literal are not braces: `views/brief.rs`'s test module searches
/// its own text for `"\n    }\n"`, and counting that `}` as the module's end cut the module
/// short — the rest of its `#[cfg(test)]` was then read as shipped code, and
/// `std::env::temp_dir()` in a test fixture was reported as a new environment read. The same
/// literal-aware count is in `tests/no_web_ui.rs`, where the same string cost the same thing.
fn without_test_modules(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    loop {
        let Some(at) = rest.find("#[cfg(test)]") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let chars: Vec<char> = after.chars().collect();
        let mut depth = 0i32;
        let mut opened = false;
        let mut end = after.len();
        let mut literal = Literal::Code;
        let mut index = 0usize;
        while index < chars.len() {
            match literal {
                Literal::Text => match chars[index] {
                    // `\"` is a quote inside the string, not the end of it.
                    '\\' => index += 1,
                    '"' => literal = Literal::Code,
                    _ => {}
                },
                Literal::Raw(hashes) => {
                    if chars[index] == '"'
                        && (1..=hashes).all(|step| chars.get(index + step) == Some(&'#'))
                    {
                        literal = Literal::Code;
                        index += hashes;
                    }
                }
                Literal::Code => match chars[index] {
                    '"' => literal = Literal::Text,
                    'r' if matches!(chars.get(index + 1), Some('"') | Some('#')) => {
                        let mut hashes = 0usize;
                        let mut probe = index + 1;
                        while chars.get(probe) == Some(&'#') {
                            hashes += 1;
                            probe += 1;
                        }
                        if chars.get(probe) == Some(&'"') {
                            literal = Literal::Raw(hashes);
                            index = probe;
                        }
                    }
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' if opened => {
                        depth -= 1;
                        if depth == 0 {
                            end = after
                                .char_indices()
                                .nth(index)
                                .map(|(byte, _)| byte + 1)
                                .unwrap_or(after.len());
                            break;
                        }
                    }
                    _ => {}
                },
            }
            index += 1;
        }
        rest = &after[end..];
    }
}

/// Where the scan stands with respect to a string literal.
enum Literal {
    Code,
    /// Inside `"…"`, where a `\` escapes the character after it.
    Text,
    /// Inside a raw string, with the number of `#` its closing quote needs.
    Raw(usize),
}

/// Every environment read in shipped code: `(file, line, the expression)`.
fn environment_reads() -> Vec<(String, usize, String)> {
    let mut found = Vec::new();
    for (file, source) in shipped() {
        let shipped_only = without_test_modules(&source);
        for (index, line) in shipped_only.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("env::var")
                || line.contains("env::vars")
                || line.contains("env!")
                || line.contains("current_exe")
                || line.contains("current_dir")
                || line.contains("temp_dir")
            {
                found.push((file.clone(), index + 1, trimmed.to_string()));
            }
        }
    }
    found
}

#[test]
fn the_window_reads_three_things_from_the_environment_and_prints_none_of_them() {
    // Measured, so the list is the tree's and not a wish: three reads in shipped code.
    //   `src/app.rs:122`      `ATLAS_MODEL_URL` — the reader's model address;
    //   `src/assets.rs:30`    `current_exe()` — where the window was started from;
    //   `src/assets.rs:39`    `current_dir()` — the project root when there is no executable
    //                         path (a `cargo run`).
    // `temp_dir()` appears only inside `#[cfg(test)]` modules (`setup.rs:319`,
    // `tools.rs:724`), which [`without_test_modules`] removes.
    let reads = environment_reads();
    let mut allowed = 0usize;
    for (file, line, text) in &reads {
        let ok = (file == "app.rs" && text.contains("env::var(\"ATLAS_MODEL_URL\")"))
            || (file == "assets.rs" && text.contains("env::current_exe()"))
            || (file == "assets.rs" && text.contains("env::current_dir()"));
        assert!(
            ok,
            "{}:{} reads the environment in a way this list does not name: {}\n\
             The shipped window may read the model address (app.rs:122) and the two \
             locations the platform owns (`current_exe`, `current_dir`). Anything else is a \
             new dependency on the reader's environment in a window that must look the same \
             on every machine; and a value it reads may be a credential, so if the read is \
             intended, add it here *and* say in the code that it is never printed.",
            file, line, text
        );
        allowed += 1;
    }
    assert_eq!(
        allowed,
        3,
        "the scan found {} environment reads; the tree ships three (see the comments above). \
         A new one must be named here with its reason, and a read that disappeared means this \
         test is looking at a tree that moved: {:?}",
        allowed,
        reads
    );
    // `env::vars`/`vars_os` would hand the process's whole environment to whatever asks: the
    // window has no business enumerating it.
    for (file, line, text) in &reads {
        assert!(
            !text.contains("vars()") && !text.contains("vars_os()"),
            "{}:{} enumerates the whole environment: {}",
            file,
            line,
            text
        );
    }
}

/// Every environment value this source binds to a name: `let address = std::env::var(...)`
/// answers `address`. Coarse by design — a name is collected when an environment read and a
/// `let` share a statement, which is how the window would use one.
fn environment_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for statement in source.split(';') {
        if !(statement.contains("env::var")
            || statement.contains("env::vars")
            || statement.contains("current_exe")
            || statement.contains("current_dir")
            || statement.contains("temp_dir"))
        {
            continue;
        }
        let Some(at) = statement.find("let ") else {
            continue;
        };
        let tail: String = statement[at + 4..]
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        let name = tail.trim_matches('_').to_string();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The argument text of every print macro in a source, with the line it starts on.
///
/// The span is taken to the matching `)` by counting parentheses. A `)` inside a string
/// literal would end the span early and *weaken* the scan; there is no such literal in the
/// shipped tree today, and the same-line check below still runs.
fn print_calls(source: &str) -> Vec<(usize, String)> {
    let macros = ["println!", "print!", "eprintln!", "eprint!", "dbg!"];
    let mut calls = Vec::new();
    let bytes = source.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        let rest = &source[at..];
        let Some(found) = macros.iter().find_map(|name| rest.find(name)) else {
            break;
        };
        let line = source[..at].matches('\n').count() + 1;
        let after_name = at + found + rest[found..].find('!').map(|n| n + 1).unwrap_or(0);
        let Some(open) = source[after_name..].find('(') else {
            at = after_name;
            continue;
        };
        let start = after_name + open;
        let mut depth = 0i32;
        let mut end = start;
        for (index, ch) in source[start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + index + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        calls.push((line, source[start..end.max(start + 1)].to_string()));
        at = end.max(at + 1);
    }
    calls
}

#[test]
fn no_shipped_print_carries_an_environment_value() {
    // The mouth half, and the reason `src/app.rs:134` exists: the status row says "chat needs
    // the local model" instead of the address, because "a configuration value can be a
    // credential (`http://alice:secret@127.0.0.1:8080`)". A `println!` goes further than a
    // label — a terminal, a scrollback, a pasted log — so the rule is absolute in shipped
    // code: no print macro carries an environment-derived value, directly or through a name
    // bound from one.
    //
    // The scan is textual: the *arguments* of every print macro (balanced parentheses) plus
    // every name a statement binds from an environment read. It does not follow a value
    // through a struct field or a function return; that limit is stated in the report.
    let mut offences: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (file, source) in shipped() {
        let shipped_only = without_test_modules(&source);
        let names = environment_names(&shipped_only);
        for (line, arguments) in print_calls(&shipped_only) {
            checked += 1;
            let mut bad = ["env::var", "env::vars", "env!", "current_exe", "current_dir", "temp_dir", "model_url()"]
                .iter()
                .any(|needle| arguments.contains(needle));
            if !bad {
                for name in &names {
                    // A whole-word match: `address` must not fire on `addresses`.
                    let mut from = 0usize;
                    while let Some(found) = arguments[from..].find(name.as_str()) {
                        let start = from + found;
                        let before = arguments[..start].chars().next_back();
                        let after = arguments[start + name.len()..].chars().next();
                        let boundary = |c: Option<char>| c.map(|c| !c.is_alphanumeric() && c != '_').unwrap_or(true);
                        if boundary(before) && boundary(after) {
                            bad = true;
                            break;
                        }
                        from = start + name.len();
                    }
                }
            }
            if bad {
                offences.push(format!("{}:{} {}", file, line, arguments.trim()));
            }
        }
    }
    assert!(
        checked > 0 || shipped().is_empty(),
        "no print macro was found in shipped code; the scan is looking at the wrong tree"
    );
    assert!(
        offences.is_empty(),
        "the window prints an environment-derived value:\n{}\n\
         A configuration value can be a credential (`src/app.rs:134` says so about \
         `ATLAS_MODEL_URL`), and a print goes to a terminal, a log and a screenshot. Print \
         the fact, not the value.",
        offences.join("\n")
    );
}
