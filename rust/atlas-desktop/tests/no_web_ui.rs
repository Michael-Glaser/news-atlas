//! The pivot, pinned: a native window with no browser in it, and no web frontend behind
//! it — asserted on the dependency graph and on the shipped source, so neither can go
//! back quietly.
//!
//! # Where the claim is written down
//!
//! * `rust/Cargo.toml`, on the `atlas-desktop` member: "The product's face: a native
//!   window (egui), no browser of any kind."
//! * `src/main.rs`, crate doc: "one process draws the archive with egui … There is no
//!   HTML, no CSS, no wasm bundle, no page and no socket for a page to talk to — a
//!   window, a GPU surface and a database."
//! * `src/lib.rs`, module table: [`views`] is "what a page looks like, one module per
//!   page", drawn with a painter; there is no document anywhere in the crate.
//! * `docs/RUST-STYLE.md`'s crate table: `atlas-web` is the server, `atlas-ui` is the
//!   wasm page, `atlas-desktop` is the window. A dependency of the window on either of
//!   the first two would be the pivot undone.
//!
//! # The four claims, and what "prove" means here
//!
//! 1. [`the_window_compiles_nothing_from_the_web`] — the *compiled* closure
//!    (`cargo tree --offline -p atlas-desktop -e normal --locked`) holds no HTTP server,
//!    no wasm binding, no HTML/template engine, and no browser driver or browser shell, and
//!    the crate's direct dependencies are the five in `Cargo.lock` and nothing else — the
//!    fifth is `wry`, the platform's own web view, which reads a story's page inside this
//!    window without starting a browser or opening a port. The
//!    closure is read from cargo's own resolution on this host, not from the workspace
//!    lock: `Cargo.lock` merges `cfg(target_arch = "wasm32")` edges, so it lists
//!    `web-sys` for a window crate that never compiles for wasm. Reading the lock instead
//!    of the tree would make this file either wrong or toothless — see
//!    [`the_lock_lies_about_the_target_and_the_tree_does_not`].
//! 2. [`no_shipped_line_speaks_the_web`] — no `.rs` file under `src/` that ships contains
//!    `innerHTML`, `<html`, `<!doctype`, `wasm_bindgen`, `web_sys`, `js_sys`, or a
//!    `text/html` content type, read as text with comments blanked (a sentence about the
//!    web is not the web) and string literals kept (a content type is a string).
//! 3. [`the_window_reads_three_store_files_and_writes_only_the_queue`] — the window names
//!    the archive, the outlines, the settings file that holds one port number, the
//!    installer's progress report and its own request queue; the archive is opened
//!    `SQLITE_OPEN_READ_ONLY`, exactly one file is written (`data/requests.db`, through
//!    the queue), and no shipped line creates, removes, moves or copies anything else.
//! 4. [`every_workspace_member_is_rust`] — every `members` entry is a directory whose
//!    `src/` holds `.rs` and nothing else (no `.c`, `.cpp`, `.py`, `.js`, `.html`).
//!
//! Each assertion names the file it read, and its message says what came back.
//!
//! # The two exceptions this file carries, both test code
//!
//! Two shipped-looking files contain a web token, and both are inside their own
//! `#[cfg(test)] mod tests`: `src/link.rs` holds the string
//! `"data:text/html,<script>alert(1)</script>"` in the test that every non-`http` scheme
//! is refused, and `src/http.rs` holds `"<html>a proxy's idea of an answer</html>"` in the
//! test that the model's reply must be JSON. They are [`ALLOWED`], pinned to a file and an
//! exact count, and [`an_allowance_is_only_an_allowance_inside_cfg_test`] proves both sit
//! below the file's `#[cfg(test)]` marker — so an allowance cannot be used to excuse a
//! line that ships. Naming them is stronger than skipping a region of the file: a *new*
//! hit anywhere, test code or not, fails, and a stale allowance fails too.
//!
//! # No exception in the graph at all
//!
//! An earlier revision of this file allowed one crate through, `webbrowser`, because
//! `src/views/articles.rs` put an `egui::Hyperlink` in a row and egui turned the click into
//! an `OpenUrl` the platform opener handled. That allowance is gone and so is the line it
//! described: the article view now copies the address to the clipboard
//! (`ui.ctx().copy_text`) and says so, and `eframe =0.37.0` with
//! `default-features = false` does not compile a URL opener — `webbrowser` is not in the
//! closure any more (110 crates on this host). The clearance is asserted, not assumed: see
//! [`the_window_compiles_nothing_from_the_web`], which fails if the crate comes back *or* if
//! an `OpenUrl`/`Hyperlink` is reintroduced in the article view.
//!
//! # Proof that it bites
//!
//! [`the_scan_bites_when_the_web_is_put_back`] plants a temp tree with an `innerHTML`
//! assignment, a `text/html` string, an `index.html` join, an `fs::write`, a
//! `CREATE TABLE`, an `src/app.js` in a member, and a `cargo tree` line for `axum` — then
//! runs the same scanners over it and asserts each rule fires on it by name. The same
//! test runs the scanners over a clean fixture that must stay silent, so a scanner that
//! fires on everything cannot pass for a scanner that works.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

// The two files whose names this file asserts, asked rather than read: a rename is then a
// failure here instead of a stale sentence in a comment.
use atlas_desktop::{assets, requests, setup};

// ---------------------------------------------------------------------------
// the tree this file reads
// ---------------------------------------------------------------------------

/// `rust/atlas-desktop` — the package whose sources are the product's face.
fn package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `rust/` — the workspace, whose `Cargo.toml` lists the members and whose `Cargo.lock`
/// the closure is read from.
fn workspace() -> PathBuf {
    package()
        .parent()
        .expect("the package is a directory inside rust/")
        .to_path_buf()
}

/// A path as a failure message should spell it: relative to `rust/` when it is inside it.
fn shown(path: &Path) -> String {
    path.strip_prefix(workspace())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} could not be read as text: {error}", shown(path)))
}

// ---------------------------------------------------------------------------
// reading source as text
// ---------------------------------------------------------------------------

/// The source with comments blanked and everything else — code *and* string literals —
/// left where it was, line for line.
///
/// Comments go because a sentence is not a capability: `src/main.rs` says "no HTML" in a
/// doc comment, and a file that explains *why* it refuses `text/html` must not be a
/// finding. Strings stay because a content type is a string: `"text/html"` written
/// anywhere is the thing this file is looking for. A naive `//`-to-end-of-line stripper
/// would blank the rest of `let u = "https://x";` and hide whatever followed it, so the
/// scanner tracks `"…"`, `r#"…"#` and `'x'` as it goes.
fn strip_comments(text: &str) -> String {
    #[derive(PartialEq)]
    enum State {
        Code,
        Line,
        Block(usize),
        Text,
        Raw(usize),
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut state = State::Code;
    let mut index = 0;
    while index < chars.len() {
        let here = chars[index];
        let next = chars.get(index + 1).copied();
        match state {
            State::Code => {
                if here == '/' && next == Some('/') {
                    state = State::Line;
                    out.push_str("  ");
                    index += 2;
                } else if here == '/' && next == Some('*') {
                    state = State::Block(1);
                    out.push_str("  ");
                    index += 2;
                } else if here == '"' {
                    state = State::Text;
                    out.push(here);
                    index += 1;
                } else if here == 'r' && matches!(next, Some('"') | Some('#')) {
                    // a raw string: `r"…"`, `r#"…"#`, `r##"…"##`
                    let mut hashes = 0;
                    let mut probe = index + 1;
                    while chars.get(probe) == Some(&'#') {
                        hashes += 1;
                        probe += 1;
                    }
                    if chars.get(probe) == Some(&'"') {
                        out.extend(&chars[index..=probe]);
                        index = probe + 1;
                        state = State::Raw(hashes);
                    } else {
                        out.push(here);
                        index += 1;
                    }
                } else if here == '\'' {
                    // `'x'`, `'\n'`, `'\u{1F600}'` are char literals; `'a` is a lifetime
                    let end = match (chars.get(index + 2), chars.get(index + 1)) {
                        (Some('\''), Some('\\')) => (0..).find(|extra| {
                            chars.get(index + 2 + extra) == Some(&'\'')
                        }),
                        (Some('\''), Some(_)) => Some(0),
                        _ => None,
                    };
                    match end {
                        Some(extra) => {
                            out.extend(&chars[index..=index + 2 + extra]);
                            index += 3 + extra;
                            state = State::Code;
                        }
                        None => {
                            out.push(here);
                            index += 1;
                        }
                    }
                } else {
                    out.push(here);
                    index += 1;
                }
            }
            State::Line => {
                if here == '\n' {
                    state = State::Code;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                index += 1;
            }
            State::Block(depth) => {
                if here == '*' && next == Some('/') {
                    state = if depth <= 1 { State::Code } else { State::Block(depth - 1) };
                    out.push_str("  ");
                    index += 2;
                } else if here == '/' && next == Some('*') {
                    state = State::Block(depth + 1);
                    out.push_str("  ");
                    index += 2;
                } else {
                    out.push(if here == '\n' { '\n' } else { ' ' });
                    index += 1;
                }
            }
            State::Text => {
                out.push(here);
                match here {
                    '\\' => {
                        if let Some(escaped) = next {
                            out.push(escaped);
                            index += 1;
                        }
                    }
                    '"' => state = State::Code,
                    _ => {}
                }
                index += 1;
            }
            State::Raw(hashes) => {
                out.push(here);
                if here == '"'
                    && hashes > 0
                    && chars[index + 1..].iter().take(hashes).filter(|c| **c == '#').count() == hashes
                {
                    out.extend(&chars[index + 1..=index + hashes]);
                    index += hashes + 1;
                    state = State::Code;
                    continue;
                }
                if here == '"' && hashes == 0 {
                    state = State::Code;
                }
                index += 1;
            }
        }
    }
    out
}

/// The 1-based lines of `text` that contain `needle`, ignoring case.
fn hits(text: &str, needle: &str) -> Vec<usize> {
    let needle = needle.to_lowercase();
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .map(|(index, _)| index + 1)
        .collect()
}

/// The 1-based lines that contain `needle` exactly — for SQL, which this codebase writes in
/// upper case: `update(`, `insert(` and `dropped` are Rust, `UPDATE` and `INSERT INTO` are
/// statements.
fn hits_exact(text: &str, needle: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(index, _)| index + 1)
        .collect()
}

/// One finding: the file it was read from, the token, the lines, and what it would mean.
struct Hit {
    path: PathBuf,
    needle: &'static str,
    lines: Vec<usize>,
    meaning: &'static str,
}

impl Hit {
    /// The sentence a failure shows: the file, the lines, the token, and why it matters.
    fn sentence(&self) -> String {
        format!(
            "{}:{} — {} (`{}`)",
            shown(&self.path),
            self.lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            self.meaning,
            self.needle
        )
    }
}

/// Every finding as a sentence, one per line.
fn sentences(found: &[Hit]) -> String {
    found.iter().map(Hit::sentence).collect::<Vec<_>>().join("\n  ")
}

/// How many times `needle` occurs in `text`, ignoring case.
fn occurrences(text: &str, needle: &str) -> usize {
    let needle = needle.to_lowercase();
    text.lines()
        .map(|line| line.to_lowercase().matches(&needle).count())
        .sum()
}

/// Every `.rs` file under `dir`, deepest last, symlinks followed and de-duplicated: the
/// crate ships one file twice (`src/tests.rs` is a symlink to `src/data/tests.rs`), and a
/// scan that counted it twice would report every hit in it twice.
fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    collect_rs(dir, &mut found, &mut seen);
    found.sort();
    found
}

fn collect_rs(dir: &Path, found: &mut Vec<PathBuf>, seen: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        // `metadata` follows a symlink, so a pointer to a file is read as that file
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            collect_rs(&path, found, seen);
        } else if path.extension() == Some(OsStr::new("rs")) {
            let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.insert(real) {
                found.push(path);
            }
        }
    }
}

/// The files that are a `#[cfg(test)] mod tests;` and nothing else.
///
/// A module declared under `#[cfg(test)]` is not compiled into the window: excluding it is
/// a structural fact about the crate, not a judgement about its contents. `src/data/tests.rs`
/// is the one such file today — the fixture archive the read-only tests build in a temp
/// directory, which is where `INSERT`, `CREATE` and `fs::create_dir_all` live.
fn test_only_files(src: &Path) -> BTreeSet<PathBuf> {
    let mut only = BTreeSet::new();
    for file in rs_files(src) {
        let text = read(&file);
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            let Some(next) = lines.get(index + 1).map(|line| line.trim()) else { continue };
            let Some(rest) = next.strip_prefix("mod ") else { continue };
            let Some(name) = rest.strip_suffix(';') else { continue };
            // Both shapes rustc accepts for a module's own file: `src/a/tests.rs` and
            // `src/a/tests/mod.rs`.
            let beside = module_dir(&file).join(format!("{name}.rs"));
            let inside = module_dir(&file).join(name).join("mod.rs");
            for candidate in [beside, inside] {
                if candidate.is_file() {
                    only.insert(std::fs::canonicalize(&candidate).unwrap_or(candidate));
                }
            }
        }
    }
    only
}

/// The directory a `mod X;` declared inside `file` resolves against, the way `rustc` does.
///
/// `src/lib.rs`, `src/main.rs` and `src/<anything>/mod.rs` hold the modules of their own
/// directory; any other file, `src/a/b.rs`, holds the modules of `src/a/b/`. Reading this
/// wrong is how a test module becomes a "finding": `src/data.rs` declares `mod tests;` and
/// rustc opens `src/data/tests.rs`, while a rule that looked for `src/tests.rs` beside
/// `src/data.rs` found nothing and scanned the test file as if it shipped. That is measured,
/// not supposed — this file was green only while `src/tests.rs` happened to be a symlink to
/// `src/data/tests.rs`, so the canonicalized paths matched by accident; the day the symlink
/// was removed, `fs::create_dir` inside a test module was reported as the window writing a
/// directory.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().map(Path::to_path_buf).unwrap_or_default();
    match file.file_stem().and_then(OsStr::to_str).unwrap_or("") {
        "lib" | "main" | "mod" => parent,
        stem => parent.join(stem),
    }
}

/// The source that ships: every `.rs` under `src/` that is not a `#[cfg(test)]` module,
/// comment-blanked, as `(path, text)`.
fn shipped(src: &Path) -> Vec<(PathBuf, String)> {
    let only = test_only_files(src);
    rs_files(src)
        .into_iter()
        .filter(|file| {
            let real = std::fs::canonicalize(file).unwrap_or_else(|_| file.clone());
            !only.contains(&real)
        })
        .map(|file| {
            // An inline `#[cfg(test)] mod … { … }` runs only in a test build, and the rules
            // below are about the source that *ships* — a test that makes a temporary file is
            // how a reader of files is tested at all, and reading it as the window's write
            // surface would turn every such test into an allowance. The module is removed
            // **block by block**, never by cutting the rest of the file: an item written after
            // the test module is still shipped code, and a `truncate at the boundary` version of
            // this line let a planted `fs::write` there pass unnoticed (measured — the plant
            // was added to `src/theme.rs` below its test module and the rule stayed green).
            let text = without_test_modules(&strip_comments(&read(&file)));
            (file, text)
        })
        .collect()
}

/// `text` with every inline `#[cfg(test)] mod … { … }` removed, and everything around it kept.
///
/// Braces are counted on the lines, skipping the ones inside a string literal: `"\n    }\n"` is
/// a string a test searches for, not the end of the module. That is measured — `src/views/brief.rs`
/// has exactly that string at line 872, the brace in it ended the module nine lines early, and the
/// six `fs::create_dir` calls further down its `#[cfg(test)]` were then read as the window making
/// directories. A brace inside a *character* literal would still be miscounted; there is none in
/// this tree, and the failure mode stays a finding (test code read as shipped), never silence.
fn without_test_modules(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let starts_a_test_module = lines[index].trim() == "#[cfg(test)]"
            && lines
                .get(index + 1)
                .map(|next| next.trim().starts_with("mod "))
                .unwrap_or(false);
        if !starts_a_test_module {
            kept.push(lines[index]);
            index += 1;
            continue;
        }
        // Skip the attribute and the module it belongs to, balanced.
        let mut depth: i32 = 0;
        let mut seen_a_brace = false;
        let mut literal = Literal::Code;
        index += 1;
        while index < lines.len() {
            let (opens, closes) = braces_outside_literals(lines[index], &mut literal);
            depth += opens as i32;
            depth -= closes as i32;
            seen_a_brace |= opens > 0;
            index += 1;
            if seen_a_brace && depth <= 0 {
                break;
            }
        }
    }
    kept.join("\n")
}

/// Where a line's scanner stands with respect to a string literal, carried between lines because
/// Rust's strings may span them.
enum Literal {
    Code,
    /// Inside `"…"`: a `\` escapes one character and ends nothing.
    Text,
    /// Inside a raw string, with the number of `#` its closing quote needs.
    Raw(usize),
}

/// One line's `(openings, closings)`, counting only braces in code.
fn braces_outside_literals(line: &str, state: &mut Literal) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    let (mut opens, mut closes) = (0usize, 0usize);
    let mut index = 0usize;
    while index < chars.len() {
        match *state {
            Literal::Text => match chars[index] {
                '\\' => index += 1,
                '"' => *state = Literal::Code,
                _ => {}
            },
            Literal::Raw(hashes) => {
                if chars[index] == '"'
                    && (1..=hashes).all(|step| chars.get(index + step) == Some(&'#'))
                {
                    *state = Literal::Code;
                    index += hashes;
                }
            }
            Literal::Code => match chars[index] {
                '"' => *state = Literal::Text,
                'r' if matches!(chars.get(index + 1), Some('"') | Some('#')) => {
                    // `r"…"`, `r#"…"#`, `r##"…"##`: the hashes decide which quote closes it.
                    let mut hashes = 0usize;
                    let mut probe = index + 1;
                    while chars.get(probe) == Some(&'#') {
                        hashes += 1;
                        probe += 1;
                    }
                    if chars.get(probe) == Some(&'"') {
                        *state = Literal::Raw(hashes);
                        index = probe;
                    }
                }
                '{' => opens += 1,
                '}' => closes += 1,
                _ => {}
            },
        }
        index += 1;
    }
    (opens, closes)
}
/// `text` with everything that is not a string literal blanked, newlines kept so a hit still
/// reports the line it was found on.
///
/// The statement rule reads *this* rather than the source, because that is how a statement
/// reaches the archive: through a string literal. A Rust name is not SQL — `views/monitors.rs`
/// names a constant `NO_DELETE` and tells a reader in its sentence that the window cannot
/// delete anything, and the source scan read that name twice as a `DELETE`. The blanking is
/// total, so a statement cannot hide in an identifier either: a literal is the only place a
/// query exists, and `no_spawn_no_socket.rs` pins the queue's single insert word for word.
fn only_strings(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '\n' {
            out.push('\n');
            index += 1;
            continue;
        }
        // `r"…"`, `r#"…"#`: the hashes say which quote ends it, and its text is a string too.
        let raw = chars[index] == 'r'
            && matches!(chars.get(index + 1), Some('"') | Some('#'))
            && {
                let mut probe = index + 1;
                while chars.get(probe) == Some(&'#') {
                    probe += 1;
                }
                chars.get(probe) == Some(&'"')
            };
        if raw {
            let mut hashes = 0usize;
            let mut probe = index + 1;
            while chars.get(probe) == Some(&'#') {
                hashes += 1;
                probe += 1;
            }
            index = probe + 1;
            while index < chars.len() {
                let here = chars[index];
                out.push(if here == '\n' { '\n' } else { here });
                index += 1;
                if here == '"' && (1..=hashes).all(|step| chars.get(index - 1 + step) == Some(&'#'))
                {
                    for _ in 0..hashes {
                        out.push('#');
                    }
                    index += hashes;
                    break;
                }
            }
            continue;
        }
        if chars[index] == '"' {
            out.push('"');
            index += 1;
            while index < chars.len() {
                let here = chars[index];
                out.push(if here == '\n' { '\n' } else { here });
                index += 1;
                match here {
                    // An escape carries the next character with it: `\"` does not end a string.
                    '\\' => {
                        if let Some(next) = chars.get(index) {
                            out.push(if *next == '\n' { '\n' } else { *next });
                            index += 1;
                        }
                    }
                    '"' => break,
                    _ => {}
                }
            }
            continue;
        }
        out.push(' ');
        index += 1;
    }
    out
}

/// The line of the file's `#[cfg(test)] mod …` — the boundary between code that ships and
/// code that only runs in a test build.
fn cfg_test_boundary(text: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len()).find(|index| {
        lines[*index].trim() == "#[cfg(test)]"
            && lines
                .get(index + 1)
                .map(|next| next.trim().starts_with("mod "))
                .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// 1. dependencies
// ---------------------------------------------------------------------------

/// A crate the window may not compile in, and what its presence would mean.
struct Forbidden {
    name: &'static str,
    means: &'static str,
}

/// Nothing that serves a page. The window is a client of one local model and a reader of
/// one archive; a server in its closure is a second front door with no authentication in
/// front of it (`docs/THREAT-MODEL.md` §5.6).
const NO_SERVER: &[Forbidden] = &[
    Forbidden { name: "axum", means: "the workspace's own HTTP server, `atlas-web`'s whole reason" },
    Forbidden { name: "hyper", means: "an HTTP implementation, the layer under a server" },
    Forbidden { name: "h2", means: "HTTP/2 framing, which only a server or a browser needs" },
    Forbidden { name: "tower-http", means: "HTTP middleware: static files, CORS, compression for a page" },
    Forbidden { name: "actix-web", means: "a web framework" },
    Forbidden { name: "actix-http", means: "a web framework's HTTP layer" },
    Forbidden { name: "actix-server", means: "a web framework's listener" },
    Forbidden { name: "warp", means: "a server built out of filters" },
    Forbidden { name: "rocket", means: "a web framework" },
    Forbidden { name: "tide", means: "a web framework" },
    Forbidden { name: "poem", means: "a web framework" },
    Forbidden { name: "salvo", means: "a web framework" },
    Forbidden { name: "tiny_http", means: "a small server, which is still a server" },
    Forbidden { name: "atlas-web", means: "this workspace's server crate: the page's front door" },
];

/// Nothing that runs in a browser or compiles for one. The window is native egui; a wasm
/// binding in its closure would be a document waiting to happen.
const NO_WASM: &[Forbidden] = &[
    Forbidden { name: "wasm-bindgen", means: "the wasm boundary: JS calls from Rust" },
    Forbidden { name: "wasm-bindgen-futures", means: "promises as an async runtime" },
    Forbidden { name: "wasm-bindgen-macro", means: "the wasm boundary's macros" },
    Forbidden { name: "wasm-bindgen-shared", means: "the wasm boundary's shared types" },
    Forbidden { name: "js-sys", means: "JavaScript's own objects, typed" },
    Forbidden { name: "web-sys", means: "the DOM, typed" },
    Forbidden { name: "gloo", means: "DOM and browser-API wrappers" },
    Forbidden { name: "console_error_panic_hook", means: "a panic hook for a browser console" },
    Forbidden { name: "yew", means: "a component framework for a document" },
    Forbidden { name: "leptos", means: "a component framework for a document" },
    Forbidden { name: "sycamore", means: "a component framework for a document" },
    Forbidden { name: "dioxus-web", means: "a component framework's web renderer" },
    Forbidden { name: "atlas-ui", means: "this workspace's wasm page: the frontend the pivot replaced" },
];

/// No HTML is produced, escaped, parsed or templated anywhere in the closure. A template
/// engine is how a native app grows a page without anyone deciding to.
const NO_TEMPLATE: &[Forbidden] = &[
    Forbidden { name: "askama", means: "compile-time HTML templates" },
    Forbidden { name: "askama_escape", means: "HTML escaping, which is a page's problem" },
    Forbidden { name: "tera", means: "a template engine" },
    Forbidden { name: "handlebars", means: "a template engine" },
    Forbidden { name: "liquid", means: "a template engine" },
    Forbidden { name: "minijinja", means: "a template engine" },
    Forbidden { name: "maud", means: "HTML written as a Rust macro" },
    Forbidden { name: "maud_macros", means: "HTML written as a Rust macro" },
    Forbidden { name: "ructe", means: "templates compiled into Rust" },
    Forbidden { name: "horrorshow", means: "HTML written as a Rust macro" },
    Forbidden { name: "yarte", means: "a template engine" },
    Forbidden { name: "ramhorns", means: "a template engine" },
    Forbidden { name: "tinytemplate", means: "a template engine" },
    Forbidden { name: "typed-html", means: "HTML written as a Rust macro" },
    Forbidden { name: "markup", means: "HTML written as a Rust macro" },
    Forbidden { name: "html5ever", means: "an HTML parser: a page to parse is a page somewhere" },
    Forbidden { name: "markup5ever", means: "an HTML parser's tokenizer" },
    Forbidden { name: "scraper", means: "CSS selectors over a parsed document" },
    Forbidden { name: "kuchiki", means: "a DOM to select from" },
    Forbidden { name: "html-escape", means: "HTML escaping" },
];

/// No browser is driven. A page read by an automation driver would start a browser the
/// reader did not ask for, and the user would be back to waiting for one.
///
/// **`wry` is not in this table any more, and that is a decision with a reason.** The window
/// reads a story's own page *in itself* (`src/webview.rs`, `Page::Reader`): `wry` is the
/// binding to the platform's web view, added on macOS and Windows only, and it opens no port
/// and starts no browser. What this table still forbids is everything that would *drive*
/// something else — a WebDriver, a DevTools protocol, a browser shell around a web frontend
/// (`tauri`), or a URL handed to the reader's browser (`webbrowser`). The compiled closure is
/// checked against this table on every run, so a *second* engine cannot arrive unnoticed.
const NO_BROWSER: &[Forbidden] = &[
    Forbidden { name: "fantoccini", means: "WebDriver automation" },
    Forbidden { name: "chromiumoxide", means: "Chrome DevTools Protocol automation" },
    Forbidden { name: "headless_chrome", means: "Chrome DevTools Protocol automation" },
    Forbidden { name: "webdriver", means: "browser automation" },
    Forbidden { name: "thirtyfour", means: "WebDriver automation" },
    Forbidden { name: "playwright", means: "browser automation" },
    Forbidden { name: "tauri", means: "a webview shell around a web frontend" },
    Forbidden { name: "web-view", means: "a second embedded webview" },
    Forbidden { name: "webkit2gtk", means: "an embedded webview engine of its own" },
    Forbidden { name: "cef", means: "an embedded browser engine" },
    Forbidden { name: "webbrowser", means: "a URL handed to the reader's browser" },
];

/// There is no allowance table here any more, and that is the assertion: this file used to
/// excuse `webbrowser` for the one `egui::Hyperlink` in `src/views/articles.rs`. egui 0.37
/// without default features compiles no URL opener, and the article view copies its address
/// instead of launching it, so every name below is forbidden outright. A crate that comes
/// back has to come back through this table, in a diff somebody reads.

/// The names `cargo tree` printed, in order, without versions.
fn tree_names(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|line| line.trim_start_matches(|c: char| " │├└─\\".contains(c)).trim())
        .filter_map(|rest| rest.split_once(' '))
        .filter(|(name, tail)| {
            !name.is_empty()
                && !name.contains(':')
                && name.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                && (tail.starts_with('v') || tail.starts_with(' '))
        })
        .map(|(name, _)| name.to_string())
        .collect()
}

/// `cargo tree --offline --locked -p atlas-desktop -e normal`, run from `rust/`: the
/// closure that is actually compiled on this host, with `cfg`-specific edges resolved.
fn compiled_closure() -> (Vec<String>, String) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .args(["tree", "--offline", "--locked", "-p", "atlas-desktop", "-e", "normal"])
        .current_dir(workspace())
        .output()
        .expect("`cargo` could not be run: the closure cannot be proved without it");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "`cargo tree --offline --locked -p atlas-desktop -e normal` failed in {}: {stderr}",
        shown(&workspace())
    );
    (tree_names(&stdout), stdout)
}

/// The dependency names `Cargo.lock` records for one package — its direct edges.
fn lock_dependencies(lock: &str, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_package = false;
    let mut in_list = false;
    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_package = false;
            in_list = false;
            continue;
        }
        if !in_package {
            if let Some(rest) = trimmed.strip_prefix("name = ") {
                in_package = rest.trim_matches('"') == name;
            }
            continue;
        }
        if trimmed.starts_with("dependencies = [") {
            in_list = true;
            continue;
        }
        if in_list {
            if trimmed == "]" {
                in_list = false;
                in_package = false;
                continue;
            }
            let entry = trimmed.trim_matches(|c| c == '"' || c == ',');
            if let Some((crate_name, _)) = entry.split_once(' ') {
                found.push(crate_name.to_string());
            } else if !entry.is_empty() {
                found.push(entry.to_string());
            }
        }
    }
    found
}

/// Every forbidden name that appears in `names`, as a sentence naming it and its origin.
fn forbidden_names(names: &[String]) -> Vec<String> {
    let sets: [(&str, &[Forbidden]); 4] = [
        ("an HTTP server", NO_SERVER),
        ("a wasm binding", NO_WASM),
        ("an HTML/template engine", NO_TEMPLATE),
        ("a browser driver or webview", NO_BROWSER),
    ];
    let mut findings = Vec::new();
    for (category, table) in sets {
        for forbidden in table {
            if names.iter().any(|name| name == forbidden.name) {
                findings.push(format!(
                    "`{}` is in atlas-desktop's compiled closure: it is {}, and {}",
                    forbidden.name, category, forbidden.means
                ));
            }
        }
    }
    findings
}

/// The pivot, proved on the graph: the window compiles nothing that serves a page, runs in
/// a browser, produces HTML, or drives one, and its direct dependencies are the five in
/// `Cargo.lock`.
///
/// **`wry`, and why it is not the thing this file forbids.** The window has its own engine
/// page (`src/webview.rs`, `Page::Reader`): a story's address is read *in this window*, with
/// no browser started and no port opened. `wry` is the Rust binding to the platform's web
/// view — the same engine an embedded view has always been — and it is added on macOS and
/// Windows only (`cfg(any(target_os = "macos", target_os = "windows"))` in the manifest), so
/// a Linux build does not compile it at all and reads pages as text instead. What stays
/// forbidden is a *server*: no HTTP server, no `wasm`, no HTML produced here, no browser
/// launched, nothing that listens. `wry` does none of those; every one of those is still in
/// the sets below.
#[test]
fn the_window_compiles_nothing_from_the_web() {
    let lock_path = workspace().join("Cargo.lock");
    let lock = read(&lock_path);

    // Direct edges first: this is the line a new dependency would be added on, and it is
    // read from the lock rather than from the manifest so the two cannot disagree.
    let mut direct = lock_dependencies(&lock, "atlas-desktop");
    direct.sort();
    assert_eq!(
        direct,
        vec!["eframe", "egui", "rusqlite", "serde_json", "wry"],
        "{}: atlas-desktop's direct dependencies changed — the window draws with egui, \
         stores in rusqlite, parses with serde_json, reads a story's own page in the \
         platform's web view (wry), and anything else is a decision this file has not seen",
        shown(&lock_path)
    );

    let (closed, raw) = compiled_closure();
    assert!(
        closed.len() > 80,
        "the closure of atlas-desktop parsed as only {} names — the parse is wrong, and a \
         wrong parse would pass this test: {}",
        closed.len(),
        raw.lines().take(5).collect::<Vec<_>>().join(" | ")
    );
    for wanted in ["atlas-desktop", "eframe", "egui", "rusqlite", "serde_json"] {
        assert!(
            closed.iter().any(|name| name == wanted),
            "`{wanted}` is missing from the parsed closure, so the parse did not read what \
             it thinks it read"
        );
    }

    let findings = forbidden_names(&closed);
    assert!(
        findings.is_empty(),
        "atlas-desktop's dependency closure has grown a web frontend ({} crates read from \
         `cargo tree --offline --locked -p atlas-desktop -e normal` in {}):\n  {}",
        closed.len(),
        shown(&workspace()),
        findings.join("\n  ")
    );

    println!(
        "atlas-desktop compiles {} crates, counted from `cargo tree`; none serves a page, \
         runs in a browser, produces HTML or drives one, and no crate is excused.",
        closed.len()
    );
}

/// The other half of "no browser": the window does not ask one to open anything. The one
/// place a stored address could become an action — an article's `url`, scraped text in a
/// local database — offers a **copy**, so a `file:` or custom scheme in a row cannot launch
/// a program. An `egui::Hyperlink` would: egui turns a click into `OpenUrl` and the backend
/// hands the string to the OS opener, which is why `webbrowser` used to be in the closure.
#[test]
fn the_window_launches_no_url() {
    let articles = package().join("src/views/articles.rs");
    let text = strip_comments(&read(&articles));
    for token in ["Hyperlink", "OpenUrl", "open_url"] {
        assert_eq!(
            occurrences(&text, token),
            0,
            "{}: `{token}` is a URL leaving this window for the operating system's opener — \
             the address is archive text and is offered as a copy, never as a launch",
            shown(&articles)
        );
    }
    assert_eq!(
        occurrences(&text, "copy_text"),
        1,
        "{}: the gated article address is put on the clipboard by exactly one \
         `ui.ctx().copy_text(…)` line, and it now appears {} times",
        shown(&articles),
        occurrences(&text, "copy_text")
    );
    let link = package().join("src/link.rs");
    let link_text = strip_comments(&read(&link));
    assert_eq!(
        occurrences(&link_text, "\"https://\""),
        2,
        "{}: the scheme gate is written twice — `openable` decides, `host` labels — and it \
         now spells `https://` {} times, so a third instance has appeared unread",
        shown(&link),
        occurrences(&link_text, "\"https://\"")
    );
}

/// Why the closure is read from `cargo tree` and not from the workspace lock: `Cargo.lock`
/// merges every `cfg` into one graph, so the window's entry there lists `web-sys` for a
/// target it never builds. Reading the lock would either fail this suite for a dependency
/// that is never compiled, or — if the names were allow-listed — stop noticing a real one.
#[test]
fn the_lock_lies_about_the_target_and_the_tree_does_not() {
    let lock_path = workspace().join("Cargo.lock");
    let lock = read(&lock_path);
    // The lock does list wasm crates somewhere, and the host tree does not compile them:
    // two facts that together are the reason this file shells out to `cargo tree`.
    assert!(
        lock.contains("name = \"web-sys\""),
        "{}: the lock no longer mentions `web-sys`, so the note above is stale",
        shown(&lock_path)
    );
    let (closed, _) = compiled_closure();
    let from_lock: Vec<String> = vec!["web-sys".to_string(), "js-sys".to_string(), "wasm-bindgen".to_string()];
    assert!(
        from_lock.iter().all(|name| !closed.contains(name)),
        "a wasm crate is in the compiled closure on this host: {}",
        from_lock
            .iter()
            .filter(|name| closed.contains(name))
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
}

// ---------------------------------------------------------------------------
// 2. the shipped source
// ---------------------------------------------------------------------------

/// A web token, what it would mean in this crate, and where the rule is written down.
struct WebToken {
    needle: &'static str,
    means: &'static str,
}

const WEB_TOKENS: &[WebToken] = &[
    WebToken {
        needle: "innerHTML",
        means: "writing markup into a document: there is no document in this window",
    },
    WebToken {
        needle: "<html",
        means: "an HTML document being produced or parsed",
    },
    WebToken {
        needle: "<!doctype",
        means: "an HTML document being produced",
    },
    WebToken {
        needle: "wasm_bindgen",
        means: "the wasm boundary: code that expects to run in a browser",
    },
    WebToken {
        needle: "web_sys",
        means: "the DOM, which a native window does not have",
    },
    WebToken {
        needle: "js_sys",
        means: "JavaScript's objects, which a native window does not have",
    },
    WebToken {
        needle: "text/html",
        means: "an HTML content type: a page's media type",
    },
];

/// A hit this file accepts, pinned to a file and an exact count.
struct Allowed {
    file: &'static str,
    needle: &'static str,
    count: usize,
    why: &'static str,
}

const ALLOWED: &[Allowed] = &[
    Allowed {
        file: "src/link.rs",
        needle: "text/html",
        count: 1,
        why: "a test fixture in the file's own `#[cfg(test)] mod tests`: the \
              `data:text/html,<script>…` URL that `every_other_scheme_is_refused` requires \
              `openable` to refuse. It is the rule's example, not a content type this \
              crate can produce",
    },
    Allowed {
        file: "src/http.rs",
        needle: "<html",
        count: 1,
        why: "a test fixture in the file's own `#[cfg(test)] mod tests`: \
              `<html>a proxy's idea of an answer</html>` handed to `answer()`, which must \
              answer \"not JSON\"",
    },
];

/// Every web token found in the given files.
fn web_hits(files: &[(PathBuf, String)]) -> Vec<Hit> {
    let mut found = Vec::new();
    for (path, text) in files {
        for token in WEB_TOKENS {
            let lines = hits(text, token.needle);
            if !lines.is_empty() {
                found.push(Hit {
                    path: path.clone(),
                    needle: token.needle,
                    lines,
                    meaning: token.means,
                });
            }
        }
    }
    found
}

/// No shipped line speaks the web: not a DOM call, not a wasm import, not an HTML document,
/// not a page's content type.
#[test]
fn no_shipped_line_speaks_the_web() {
    let src = package().join("src");
    let files = shipped(&src);
    assert!(
        files.len() > 15,
        "only {} source files were read under {} — the walk is broken and an empty walk \
         would pass this test",
        files.len(),
        shown(&src)
    );

    let findings: Vec<Hit> = web_hits(&files)
        .into_iter()
        .filter(|hit| {
            !ALLOWED.iter().any(|allowed| {
                hit.needle == allowed.needle && hit.path.ends_with(allowed.file)
            })
        })
        .collect();
    assert!(
        findings.is_empty(),
        "the shipped source has grown a web frontend ({} files read under {}, comments \
         blanked and string literals kept):\n  {}",
        files.len(),
        shown(&src),
        sentences(&findings)
    );

    println!(
        "{} shipped source files under {} hold none of {}: {}",
        files.len(),
        shown(&src),
        WEB_TOKENS.len(),
        WEB_TOKENS
            .iter()
            .map(|token| token.needle)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// The two allowances are exactly what they claim: the pinned count, and inside the file's
/// `#[cfg(test)]` region — so no allowance can excuse a line that ships.
#[test]
fn an_allowance_is_only_an_allowance_inside_cfg_test() {
    for allowed in ALLOWED {
        let path = package().join(allowed.file);
        let raw = read(&path);
        let text = strip_comments(&raw);
        let found = occurrences(&text, allowed.needle);
        assert_eq!(
            found, allowed.count,
            "{}: `{}` is allowed {} time(s) with a reason and was found {found} — the \
             allowance is stale; re-read the fixture and update it",
            shown(&path),
            allowed.needle,
            allowed.count
        );
        let boundary = cfg_test_boundary(&raw).unwrap_or_else(|| {
            panic!(
                "{}: an allowance is claimed to be inside `#[cfg(test)]` and the file has no \
                 `#[cfg(test)] mod` at all",
                shown(&path)
            )
        });
        let lines: Vec<&str> = text.lines().collect();
        let first = hits(&text, allowed.needle)
            .into_iter()
            .next()
            .unwrap_or_default();
        assert!(
            first > boundary,
            "{}:{first}: `{}` is allowed as a test fixture but sits above the file's \
             `#[cfg(test)] mod` on line {} — that is shipped code wearing an allowance",
            shown(&path),
            allowed.needle,
            boundary + 1
        );
        assert!(
            lines.iter().any(|line| line.trim() == "#[test]"),
            "{}: the allowance says `#[cfg(test)] mod tests`, and there is no `#[test]` in \
             the file",
            shown(&path)
        );
        println!("{}: `{}` ×{} — {}", shown(&path), allowed.needle, found, allowed.why);
    }
}

// ---------------------------------------------------------------------------
// 3. the two files it reads, and the none it writes
// ---------------------------------------------------------------------------

/// A call that would write, create or remove a file, and why the window may not make it.
const WRITES: &[(&str, &str)] = &[
    ("fs::write", "a file this window wrote"),
    ("fs::create_dir", "a directory this window made"),
    ("fs::remove_", "a file this window deleted"),
    ("fs::rename", "a file this window moved"),
    ("fs::copy", "a file this window copied"),
    ("fs::set_permissions", "a mode this window changed"),
    ("fs::hard_link", "a second name for a file"),
    ("File::create", "a file opened for writing"),
    ("OpenOptions", "a way around opening a file for writing"),
    ("create_new", "creating a file that did not exist"),
];

/// A statement that would change the archive, in the upper case this codebase writes SQL in.
/// Matched exactly, because the same words in lower case are Rust: `fn update(`, `index.insert(`,
/// `dropped the headland`.
const STATEMENTS: &[(&str, &str)] = &[
    ("INSERT", "a row written into the archive"),
    ("UPDATE", "a row changed in the archive"),
    ("DELETE", "a row removed from the archive"),
    ("CREATE", "a table or index created"),
    ("DROP", "a table or index destroyed"),
    ("ALTER", "a schema changed under the reader"),
];

/// The two-word shapes of the same statements, in either case, so a lower-case `insert into`
/// is not a way past the rule above.
const STATEMENT_SHAPES: &[(&str, &str)] = &[
    ("insert into", "a row written into the archive"),
    ("delete from", "a row removed from the archive"),
    ("create table", "a table created"),
    ("create index", "an index created"),
    ("create view", "a view created"),
    ("drop table", "a table destroyed"),
    ("alter table", "a schema changed"),
];

/// Every write or statement found in the given files.
fn write_hits(files: &[(PathBuf, String)]) -> Vec<Hit> {
    let mut found = Vec::new();
    for (path, text) in files {
        // `src/requests.rs` is the window's ONE writable file, and this is the reason in one
        // place: the shipped window cannot ingest (the archive is opened
        // `SQLITE_OPEN_READ_ONLY` with `PRAGMA query_only`), so a reader's "go and look" has
        // nowhere to happen except a *request* — one row in `data/requests.db` that the
        // supervised agent claims. It makes `data/` on the way, which is the one directory
        // call allowed here. Everything else in [`WRITES`] still fires everywhere, including
        // in this file, so a second write target cannot appear quietly;
        // `no_spawn_no_socket.rs` owns the rigorous version (the queue is the only read-write
        // open in the shipped crate, with a planted regression that shows the rules fire).
        let is_the_queue = path.file_name().and_then(|name| name.to_str()) == Some("requests.rs");
        for (needle, means) in WRITES {
            // `requests.rs` also tightens the mode of the queue file it just created
            // (`set_permissions(file, 0o600)`): SQLite makes `data/requests.db` 0644
            // under the usual umask, and the rows are the reader's own words. A chmod
            // of the one file the window owns is part of writing that file, not a
            // second write target — and it is the *tighter* direction. The rigorous
            // rule (`no_spawn_no_socket.rs`: the queue is the crate's only read-write
            // open) is unaffected, because a mode is not an open.
            if is_the_queue && matches!(*needle, "fs::create_dir" | "fs::set_permissions") {
                continue;
            }
            let lines = hits_exact(text, needle);
            if !lines.is_empty() {
                found.push(Hit {
                    path: path.clone(),
                    needle,
                    lines,
                    meaning: means,
                });
            }
        }
    }
    found
}

/// Every statement-shaped hit: the archived key words exactly, and their two-word shapes in
/// either case.
fn statement_hits(files: &[(PathBuf, String)]) -> Vec<Hit> {
    let mut found = Vec::new();
    for (path, text) in files {
        // The queue's own SQL is not the archive's: `src/requests.rs` inserts one row into
        // `requests` and creates that one table, which is the window's single write and is not
        // a change to `data/atlas.db`. Skipping the file here is safe because the *shape* of
        // what it may write is pinned, narrowly, elsewhere: `no_spawn_no_socket.rs`
        // (`the_queue_is_the_only_writer_and_the_archive_stays_read_only`) requires exactly one
        // `INSERT INTO requests` in the shipped crate and no `UPDATE`, `DELETE FROM`,
        // `DROP TABLE` or `ALTER TABLE` anywhere in it. A statement added to the queue's file
        // that is not that one insert fails there, not here.
        if path.file_name().and_then(|name| name.to_str()) == Some("requests.rs") {
            continue;
        }
        // Statements are read out of the *strings* only: SQL reaches SQLite through a string
        // literal, and a Rust name is not a statement. Measured — `views/monitors.rs`'s
        // `NO_DELETE` (the constant holding the sentence "This window cannot delete a monitor…")
        // was reported twice as a `DELETE` from the archive, which is the name of a sentence
        // about a read-only window, not a write.
        let statements = only_strings(text);
        for (needle, means) in STATEMENTS {
            let lines = hits_exact(&statements, needle);
            if !lines.is_empty() {
                found.push(Hit {
                    path: path.clone(),
                    needle,
                    lines,
                    meaning: means,
                });
            }
        }
        for (needle, means) in STATEMENT_SHAPES {
            let lines = hits(&statements, needle);
            if !lines.is_empty() {
                found.push(Hit {
                    path: path.clone(),
                    needle,
                    lines,
                    meaning: means,
                });
            }
        }
    }
    found
}

/// The `std::fs` functions the shipped source calls, by name.
/// Every `std::fs::<name>` token in the window's source, with the file that first
/// named it. The path matters because the queue's own mode call is allowed and the
/// same call in a view would not be.
fn fs_functions(files: &[(PathBuf, String)]) -> std::collections::BTreeMap<String, PathBuf> {
    let mut found = std::collections::BTreeMap::new();
    for (path, text) in files {
        for line in text.lines() {
            let mut rest = line;
            while let Some(at) = rest.find("fs::") {
                let tail = &rest[at + 4..];
                let name: String = tail
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    found.entry(name).or_insert_with(|| path.clone());
                }
                rest = tail;
            }
        }
    }
    found
}

/// A literal that is a *separator*, not a name: `Vec::join` and `Path::join` share a
/// spelling, and `src/assets.rs:69` builds one relative path out of its components with
/// `parts.join("/")` — the helper that shortens a path for a caption. A separator opens no
/// file, so it is counted here rather than pinned among the names, and a second one is a
/// finding: the gap cannot be used twice.
const SEPARATORS: &[&str] = &["/"];

/// Every literal `assets.rs` joins onto a path: the names the window knows how to reach,
/// and separately every separator it joins *with*, so neither list can grow unnoticed.
fn named_files(assets_source: &str) -> (BTreeSet<String>, Vec<String>) {
    let mut found = BTreeSet::new();
    let mut separators = Vec::new();
    for line in assets_source.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("join(\"") {
            let tail = &rest[at + 6..];
            let Some(end) = tail.find('"') else { break };
            let name = &tail[..end];
            if SEPARATORS.contains(&name) {
                separators.push(name.to_string());
            } else {
                found.insert(name.to_string());
            }
            rest = &tail[end..];
        }
    }
    (found, separators)
}

/// Every store-shaped file name the shipped crate contains: a string literal ending in
/// `.db`, `.json` or `.geojson`, reduced to its base name.
///
/// Base name, not path: `"data/atlas.db"` and `"atlas.db"` are the same file, and a rule
/// that counted spellings would pass a second `join("data")` in a new module. A `format!`
/// template (`atlas-tools-{}-{name}.db`) is a name this cannot resolve, so it is a *finding*
/// rather than a pass: a path built at run time is exactly what this list exists to catch.
fn store_names(files: &[(PathBuf, String)]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (_, text) in files {
        for line in text.lines() {
            let mut rest = line;
            while let Some(open) = rest.find('"') {
                let tail = &rest[open + 1..];
                let Some(end) = tail.find('"') else { break };
                let literal = &tail[..end];
                let base = literal.rsplit('/').next().unwrap_or(literal);
                if base.ends_with(".db") || base.ends_with(".json") || base.ends_with(".geojson") {
                    found.insert(base.to_string());
                }
                rest = &tail[end + 1..];
            }
        }
    }
    found
}

/// The window's whole filesystem surface, stated as it is: three store files read, one
/// queue written.
///
/// # Why the rule says this, and not "two files and no writes"
///
/// The first version of this test said *two files, and no writes at all*: the archive and
/// the outlines, plus the settings file for one port number. That was true while the window
/// was only a reader. It stopped being true, and a rule that keeps saying it is not a strict
/// rule — it fails for the wrong reason, and the next person relaxes it to get a green run.
/// The window now also reads `data/setup.json` (the installer's progress line,
/// `src/setup.rs`) and writes `data/requests.db` (the request queue and the license
/// acceptance row, `src/requests.rs`). So the rule states exactly that, and nothing wider:
///
/// * **Read** — `data/atlas.db` (read-only), `data/config.json` (one port number),
///   `data/setup.json` (one progress line), plus the vendored outlines and the model
///   manifest that `src/assets.rs` already pins by name.
/// * **Written** — `data/requests.db`, only that file, and only through the queue: one
///   connection per call, opened `READ_WRITE | CREATE`, with the five statements pinned
///   below and no `UPDATE`, `DELETE`, `DROP`, `ALTER` or `ATTACH` anywhere in the shipped
///   crate.
/// * **Never** — a listening socket, a web asset, a script. The store modules are read as
///   data: no `TcpListener`/`bind(`, no `.html`/`text/html`/`innerHTML`/`<script>`, no wasm
///   binding. The crate-wide halves of that claim are [`no_shipped_line_speaks_the_web`] and
///   `tests/no_spawn_no_socket.rs`; the assertion here is scoped to the four modules this
///   rule enumerates, so a store file that started rendering or listening fails twice.
///
/// Every name below is either asked of the crate (`assets::archive`, `setup::path`,
/// `requests::queue_path`) or counted in the shipped source, so a rename fails here instead
/// of rotting in a comment.
#[test]
fn the_window_reads_three_store_files_and_writes_only_the_queue() {
    // The names, asked of the crate rather than read out of a comment.
    let archive = assets::archive();
    assert!(
        archive.ends_with("data/atlas.db"),
        "assets::archive() answered {} — the archive is `data/atlas.db` in the checkout root, \
         and it is the one file the window reads rows from",
        archive.display()
    );
    let outlines = assets::geojson();
    let name = outlines
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(
        ["ne_50m_countries.geojson", "ne_50m_countries.geo.json", "countries.geo.json"]
            .contains(&name.as_str()),
        "assets::geojson() answered {} — the outlines are one of the three vendored \
         spellings of the Natural Earth file, and this is neither",
        outlines.display()
    );
    assert!(
        outlines.parent().map(|dir| dir.ends_with("vendor")).unwrap_or(false),
        "assets::geojson() answered {} — the outlines live in `public/vendor/`, and a \
         path outside it is a different file",
        outlines.display()
    );
    let settings = assets::config();
    assert!(
        settings.ends_with("data/config.json"),
        "assets::config() answered {} — the settings file is `data/config.json`, read for \
         one port number and nothing else",
        settings.display()
    );

    // The installer's progress report: `<root>/data/setup.json`, the file `atlas-setup`
    // writes while it works. The window *reads* it — one line of progress on the Chat page —
    // and never writes it, which is why it is here and not among the writes below.
    let report = setup::path(&archive);
    assert_eq!(
        report,
        assets::root().join("data").join(setup::SETUP_FILE),
        "setup::path(&archive) answered {} — the report is `{}` beside the archive, and a \
         second spelling of it is a second file",
        report.display(),
        setup::SETUP_FILE
    );
    assert!(
        report.ends_with("data/setup.json"),
        "setup::path(&archive) answered {} — the report is `data/setup.json` in the checkout \
         root, read for one line of progress",
        report.display()
    );

    // The request queue: the window's **one writable file**. The supervised agent claims
    // rows from it; the window only ever adds one (a `research`/`monitor_run`/`topic_add`/
    // `enrich` request, or the license acceptance row, which shares the file on purpose).
    let queue = requests::queue_path();
    assert_eq!(
        queue,
        assets::root().join("data").join("requests.db"),
        "requests::queue_path() answered {} — the queue is `data/requests.db` beside the \
         archive, and a second spelling of it is a second write target",
        queue.display()
    );
    assert!(
        queue.ends_with("data/requests.db"),
        "requests::queue_path() answered {} — the queue is `data/requests.db` in the \
         checkout root",
        queue.display()
    );
    assert_ne!(
        queue, archive,
        "the queue and the archive are the same path — the window would be writing its own \
         evidence"
    );

    // The complete set of names the file builder can produce: a third kind of file is a
    // finding, whatever it is called.
    let assets_path = package().join("src/assets.rs");
    let assets_source = strip_comments(&read(&assets_path));
    let (named, separators) = named_files(&assets_source);
    assert_eq!(
        separators.iter().map(String::as_str).collect::<Vec<_>>(),
        SEPARATORS.to_vec(),
        "{}: the path builder joins {} separator literal(s) — `parts.join(\"/\")` is the one \
         this file knows (a `Vec::join`, not a segment under the root), and anything else \
         here is a shape the scan has not been taught",
        shown(&assets_path),
        separators.len()
    );
    let expected: BTreeSet<String> = [
        "rust",          // the checkout marker: `rust/Cargo.toml`
        "Cargo.toml",    // …the marker itself
        "data",          // the archive's and the settings file's directory
        "atlas.db",      // the archive
        "config.json",   // the settings file
        "public",        // the outlines' directory
        "vendor",        // …and its subdirectory
        "ne_50m_countries.geojson",
        "ne_50m_countries.geo.json",
        "countries.geo.json",
        // Added with the first-run report (`assets::Piece`): a machine that has never run
        // `atlas-setup` has to be told *which* piece is missing, and these are the three
        // names that answer it. `bin` is where the model server binary is expected,
        // `models` is where the weights go, and `model.json` is the manifest that names
        // the file, its size and its digest. None of them is opened: the report stats a
        // directory and reads one tiny manifest, and the 6.5 GB of weights are never
        // touched by this window (see `tests/disclosure.rs` for the path rule).
        "bin",
        "models",
        "model.json",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let strangers: Vec<&String> = named.difference(&expected).collect();
    assert!(
        strangers.is_empty(),
        "{}: the window names {} file(s) it did not before: {} — the list is pinned in this \
         test, so a new name has to be argued for here",
        shown(&assets_path),
        strangers.len(),
        strangers
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Read-only: the archive is opened with the flag that makes SQLite refuse a write,
    // and the flag that would allow one is nowhere in the crate.
    let src = package().join("src");
    let files = shipped(&src);
    let data_path = package().join("src/data.rs");
    let data_source = strip_comments(&read(&data_path));
    assert!(
        data_source.contains("SQLITE_OPEN_READ_ONLY"),
        "{}: the archive is no longer opened `SQLITE_OPEN_READ_ONLY` — every page reads it, \
         so an open that can write is a write waiting for a bug",
        shown(&data_path)
    );
    for flag in ["SQLITE_OPEN_READ_WRITE", "SQLITE_OPEN_CREATE"] {
        let found = occurrences(&data_source, flag);
        assert_eq!(
            found, 0,
            "{}: `{flag}` appears {found} time(s) — the window may open the archive and \
             never create it or write to it",
            shown(&data_path)
        );
    }

    // ..and it is the *only* file opened for writing. One occurrence of the read-write
    // flag, in the queue's own module: the flag that would let a second file be written is
    // not anywhere else in the shipped crate, whatever a page asks for.
    let writable: Vec<String> = files
        .iter()
        .filter(|(_, text)| text.contains("SQLITE_OPEN_READ_WRITE"))
        .map(|(path, _)| shown(path))
        .collect();
    assert_eq!(
        writable,
        vec![shown(&package().join("src/requests.rs"))],
        "the shipped source opens {} file(s) read-write: {} — exactly one is allowed, the \
         request queue in `src/requests.rs`",
        writable.len(),
        writable.join(", ")
    );

    // The queue's statements, pinned one by one: the window creates its own two tables and
    // one index, inserts a request, and records the license acceptance in the same file. An
    // `UPDATE` is not among them — the queue's rows belong to the agent once written, and
    // the acceptance record is idempotent by `INSERT OR REPLACE` instead, which is why the
    // rule can forbid an update anywhere in the crate and still hold.
    let requests_path = package().join("src/requests.rs");
    // Shipped lines only: `src/requests.rs`'s own `#[cfg(test)] mod tests` asserts that
    // `SCHEMA` starts with the same `CREATE TABLE …` prefix this rule counts, so counting
    // the whole file made the pin read 2 where one statement ships — the pin was counting a
    // test's assertion as a second table. `without_test_modules` is the same filter every
    // other rule in this file reads the source through (`shipped`), so a test can neither
    // hide a statement nor invent one.
    let requests_source = without_test_modules(&strip_comments(&read(&requests_path)));
    for (statement, count) in [
        ("CREATE TABLE IF NOT EXISTS requests (", 1),
        ("CREATE TABLE IF NOT EXISTS acceptance (", 1),
        ("CREATE INDEX IF NOT EXISTS requests_open", 1),
        ("INSERT INTO requests (kind, payload, created_at)", 1),
        ("INSERT OR REPLACE INTO acceptance", 1),
    ] {
        let found = occurrences(&requests_source, statement);
        assert_eq!(
            found, count,
            "{}: `{statement}` appears {found} time(s), not {count} — the queue's schema and \
             its two writes are pinned here, so a table, a statement or a second write target \
             has to be argued for in this test",
            shown(&requests_path)
        );
    }
    for forbidden in ["UPDATE", "DELETE FROM", "DROP TABLE", "ALTER TABLE", "ATTACH ", "PRAGMA writable_schema"] {
        let found = occurrences(&requests_source, forbidden);
        assert_eq!(
            found, 0,
            "{}: `{forbidden}` appears {found} time(s) — the queue adds rows and records one \
             acceptance; it never rewrites, deletes or re-schemas anything",
            shown(&requests_path)
        );
    }

    // Every write goes through the one path builder: the three public entry points delegate
    // to their `_in` form with `queue_path()`, so there is no second file to pass in.
    for call in [
        "push_to(&queue_path(), kind, payload)",
        "accepted_in(&queue_path(), what)",
        "accept_in(&queue_path(), what)",
    ] {
        let found = occurrences(&requests_source, call);
        assert_eq!(
            found, 1,
            "{}: `{call}` appears {found} time(s), not 1 — the queue's public entry points \
             are the only callers of the read-write open, and they all name `queue_path()`",
            shown(&requests_path)
        );
    }

    // No other store file: every `.db` / `.json` / `.geojson` literal the shipped crate holds
    // is one of the eight names below. A ninth is a path this window can reach, and it has to
    // be argued for here rather than appearing in a page.
    let names = store_names(&files);
    let expected_names: BTreeSet<String> = [
        "atlas.db",                    // the archive, read-only
        "config.json",                 // one port number
        "setup.json",                  // the installer's progress line
        "requests.db",                 // the queue: the one write
        "model.json",                  // the model manifest, for the first-run report
        "ne_50m_countries.geojson",    // the vendored outlines, three spellings
        "ne_50m_countries.geo.json",
        "countries.geo.json",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let strangers: Vec<&String> = names.difference(&expected_names).collect();
    assert!(
        strangers.is_empty(),
        "the shipped source names {} store file(s) that are not in the list this test pins: \
         {} — the window reads the archive, the settings, the setup report and the outlines, \
         and writes only the queue",
        strangers.len(),
        strangers
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // Both halves: a ninth name is a finding, and a pinned name the source no longer holds is
    // a stale pin. Without the second half this rule would pass on an empty set.
    let missing: Vec<&String> = expected_names.difference(&names).collect();
    assert!(
        missing.is_empty(),
        "{} pinned store file(s) are no longer named by the shipped source: {} — a stale pin \
         is how this list stops describing the crate",
        missing.len(),
        missing
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // And the store modules stay data: no listener, no markup, no script. The crate-wide
    // versions of this rule are `no_shipped_line_speaks_the_web` and
    // `tests/no_spawn_no_socket.rs`; this one is scoped to the four modules above, so a
    // store file that started serving or rendering fails in the rule that enumerates it.
    const NEVER: &[&str] = &[
        "TcpListener",
        "UdpSocket",
        "UnixListener",
        "bind(",
        "axum",
        "text/html",
        "<html",
        "<script",
        ".html",
        "innerHTML",
        "wasm_bindgen",
        "web_sys",
    ];
    let mut refused: Vec<Hit> = Vec::new();
    for (path, text) in &files {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        if !["assets.rs", "data.rs", "requests.rs", "setup.rs"].contains(&name) {
            continue;
        }
        for needle in NEVER {
            let lines = hits_exact(text, needle);
            if !lines.is_empty() {
                refused.push(Hit {
                    path: path.clone(),
                    needle,
                    lines,
                    meaning: "a store module that serves or renders",
                });
            }
        }
    }
    assert!(
        refused.is_empty(),
        "a module that only reads and queues has a web or socket token in it:\n  {}",
        sentences(&refused)
    );

    let writes = write_hits(&files);
    assert!(
        writes.is_empty(),
        "the shipped source writes a file ({} files read under {}):\n  {}",
        files.len(),
        shown(&src),
        sentences(&writes)
    );
    let statements = statement_hits(&files);
    assert!(
        statements.is_empty(),
        "the shipped source carries a statement that would change the archive ({} files \
         read under {}):\n  {}",
        files.len(),
        shown(&src),
        sentences(&statements)
    );

    let fs = fs_functions(&files);
    // The file calls, and no others: reading a small text file (`read_to_string` — the
    // outlines, the settings, the model manifest), `metadata` (a size, for the cap and for the
    // first-run report), and one `read_dir` of `models/` so the newest weights file can be
    // *named* to a reader who has more than one.
    //
    // `create_dir_all` is the fourth, and it is the queue's: the window's one writable file is
    // `data/requests.db` (`src/requests.rs`), and a request that failed because `data/` did not
    // exist yet would be a button that works only on a machine that has already run the agent.
    // It is the same allowance this file already makes in `write_hits`, and the rigorous half
    // is `no_spawn_no_socket.rs`'s: one read-write open, one insert, nothing else writes.
    // `File` is the fifth, and it is a *read*: the settings page checks the first four bytes
    // of a model file the reader pasted (`settings.rs`'s `check_import`) and refuses a file
    // that does not begin `GGUF` before anything is queued, which is the one thing
    // `read_to_string` cannot do — a weights file is not text, and reading a 4 GB file to
    // look at four bytes is not a check. The write half of `File` is not covered by this
    // list at all: `File::create`, `OpenOptions` and `create_new` are in [`WRITES`] above,
    // so opening one for writing fails there whatever this set says.
    let allowed: BTreeSet<String> =
        ["read_to_string", "metadata", "read_dir", "create_dir_all", "File"]
            .into_iter()
            .map(str::to_string)
            .collect();
    // The queue's mode is the sixth, and it is scoped to the file that owns it:
    // `requests.rs` calls `set_permissions(file, 0o600)` on the queue it just created,
    // because SQLite makes `data/requests.db` 0644 under the usual umask while the rows
    // are the reader's own words. `Permissions`/`PermissionsExt` are the *type* the mode
    // is built from. A mode is not an open, and the same call in any other file is still
    // a finding — which is why this map keeps the path instead of a bare name.
    let mode_allowed_in = |path: &PathBuf| {
        path.file_name().and_then(|name| name.to_str()) == Some("requests.rs")
    };
    let mut strangers: Vec<String> = Vec::new();
    for (name, path) in &fs {
        let is_mode = matches!(
            name.as_str(),
            "set_permissions" | "Permissions" | "PermissionsExt"
        );
        if allowed.contains(name) || (is_mode && mode_allowed_in(path)) {
            continue;
        }
        strangers.push(format!("std::fs::{name} at {}", path.display()));
    }
    assert!(
        strangers.is_empty(),
        "{}: the window calls {} — the only file calls this crate may make are \
         `read_to_string` (the outlines, the settings), `metadata` (the size cap), `read_dir` \
         (naming the weights), `create_dir_all` (the request queue's own directory), `File` \
         (four bytes of a model file, to refuse a file that is not a GGUF) and the queue's \
         own `set_permissions` in `requests.rs`, so anything else is a new kind of file \
         access",
        shown(&src),
        strangers.join(", ")
    );

    println!(
        "reads {} (read-only), {} (one port) and {} (setup progress), plus {}; writes {} \
         only; {} shipped files, fs calls: {}",
        archive.display(),
        settings.display(),
        report.display(),
        outlines.display(),
        queue.display(),
        files.len(),
        fs.keys().cloned().collect::<Vec<_>>().join(", ")
    );
}

// ---------------------------------------------------------------------------
// 4. every member is Rust
// ---------------------------------------------------------------------------

/// The `members` of a workspace manifest, in order.
fn members(manifest: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if !inside {
            if trimmed.starts_with("members = [") {
                inside = true;
            }
            continue;
        }
        if trimmed.starts_with(']') {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix('"') {
            if let Some((name, _)) = rest.split_once('"') {
                found.push(name.to_string());
            }
        }
    }
    found
}

/// Extensions that mean a member carries source in another language, or a web asset.
const NOT_RUST: &[&str] = &[
    "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "m", "mm", "py", "js", "mjs", "ts", "tsx", "jsx",
    "html", "htm", "css", "scss", "sass", "wasm", "go", "rb", "java", "kt", "swift", "sh", "bat",
    "ps1",
];

/// Every file under `dir` whose extension is not Rust.
fn non_rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.is_dir() {
                pending.push(path);
            } else if let Some(ext) = path.extension().and_then(OsStr::to_str) {
                if NOT_RUST.contains(&ext.to_ascii_lowercase().as_str()) {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    found
}

/// No member's `src/` holds anything but Rust: the answer to \"100 % Rust\" is a scan of
/// every crate the workspace builds, not a promise about the one this file belongs to.
#[test]
fn every_workspace_member_is_rust() {
    let manifest_path = workspace().join("Cargo.toml");
    let manifest = read(&manifest_path);
    let members = members(&manifest);
    assert!(
        members.len() > 20,
        "{}: only {} members parsed — the workspace is bigger than that, and a short list \
         would skip crates",
        shown(&manifest_path),
        members.len()
    );
    assert!(
        members.iter().all(|member| !member.starts_with("tools")),
        "{}: a `tools/` path is in `members` — `tools/llama.cpp` is vendored C++ that is \
         built by the setup crate, not a workspace member",
        shown(&manifest_path)
    );

    for member in &members {
        let src = workspace().join(member).join("src");
        assert!(
            src.is_dir(),
            "{}: member `{member}` has no `src/`, so the scan below would have skipped it \
             without saying so",
            shown(&manifest_path)
        );
        let strangers = non_rust_files(&src);
        assert!(
            strangers.is_empty(),
            "{}: member `{member}` carries {} file(s) that are not Rust — {}",
            shown(&manifest_path),
            strangers.len(),
            strangers
                .iter()
                .map(|path| shown(path))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    println!(
        "{} members under {}: every `src/` holds Rust and nothing else",
        members.len(),
        shown(&workspace().join("Cargo.toml"))
    );
}

// ---------------------------------------------------------------------------
// the planted proof
// ---------------------------------------------------------------------------

/// A temp tree, removed when the test is done with it.
struct Planted {
    root: PathBuf,
}

impl Planted {
    fn new(name: &str) -> Planted {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|span| span.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "atlas-no-web-ui-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("a temp directory for the planted tree");
        Planted { root }
    }

    fn write(&self, at: &str, text: &str) -> PathBuf {
        let path = self.root.join(at);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a planted directory");
        std::fs::write(&path, text).expect("a planted file");
        path
    }
}

impl Drop for Planted {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The scanners above, run over a planted tree: each rule must fire, by name, on the file
/// that breaks it — and stay silent on the clean fixture beside it.
#[test]
fn the_scan_bites_when_the_web_is_put_back() {
    let planted = Planted::new("planted");

    // a document written from Rust, and a page's content type
    let web = planted.write(
        "src/web.rs",
        "fn draw(document: &Document, el: &Element) {\n    el.set_innerHTML(\"<p>hi</p>\");\n}\n\
         const TYPE: &str = \"text/html\";\nconst DOCTYPE: &str = \"<!doctype html>\";\n",
    );
    // a third kind of file, named by the path builder
    let assets = planted.write(
        "src/assets.rs",
        "pub fn page() -> PathBuf { root().join(\"public\").join(\"index.html\") }\n",
    );
    // a write, and a statement that would change the archive
    let db = planted.write(
        "src/db.rs",
        "fn save(path: &Path) {\n    std::fs::write(path, b\"x\").expect(\"w\");\n}\n\
         fn schema(conn: &Connection) {\n    conn.execute(\"CREATE TABLE x(y)\", []);\n\
         conn.execute(\"insert into x values (1)\", []);\n}\n",
    );
    // a store file the window has no business naming, opened for writing
    let store = planted.write(
        "src/store.rs",
        "fn cache() -> PathBuf {\n    \"data/analytics.db\".into()\n}\n\
         fn open(file: &Path) -> Connection {\n    \
         Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap()\n}\n",
    );
    // a member carrying JavaScript
    let script = planted.write("members/atlas-webby/src/app.js", "document.body.innerHTML = 1;\n");
    let _ = planted.write("members/atlas-webby/src/lib.rs", "\n");
    let manifest = planted.write("Cargo.toml", "[workspace]\nmembers = [\n    \"members/atlas-webby\",\n]\n");
    // a graph with a server in it, and the lock's direct edges
    let tree = "atlas-desktop v0.1.0\n├── axum v0.8.1\n└── webbrowser v1.2.7\n";
    let lock = "[[package]]\nname = \"atlas-desktop\"\nversion = \"0.1.0\"\ndependencies = [\n \"axum\",\n]\n\n";

    // 1. the dependence on a server
    let names = tree_names(tree);
    let findings = forbidden_names(&names);
    assert!(
        findings.iter().any(|finding| finding.contains("`axum`")),
        "the closure scanner did not fire on a planted `axum`: it read {names:?} and reported \
         {findings:?}"
    );
    assert_eq!(
        lock_dependencies(lock, "atlas-desktop"),
        vec!["axum".to_string()],
        "the lock reader did not find the planted direct dependency"
    );

    // 2. the shipped source
    let src = planted.root.join("src");
    let files = shipped(&src);
    assert_eq!(files.len(), 4, "the walk read {files:?}");
    let findings = web_hits(&files);
    for needle in ["innerHTML", "text/html", "<!doctype"] {
        assert!(
            findings
                .iter()
                .any(|hit| hit.needle == needle && hit.path.ends_with("src/web.rs")),
            "the source scan did not fire on a planted `{needle}` in {}: it reported {}",
            shown(&web),
            sentences(&findings)
        );
    }

    // 3. a write, a statement, and a third file
    let writes = write_hits(&files);
    assert!(
        writes
            .iter()
            .any(|hit| hit.needle == "fs::write" && hit.path.ends_with("src/db.rs")),
        "the write scan did not fire on a planted `fs::write` in {}: it reported {}",
        shown(&db),
        sentences(&writes)
    );
    let statements = statement_hits(&files);
    for needle in ["CREATE", "insert into"] {
        assert!(
            statements
                .iter()
                .any(|hit| hit.needle == needle && hit.path.ends_with("src/db.rs")),
            "the statement scan did not fire on a planted `{needle}` in {}: it reported {}",
            shown(&db),
            sentences(&statements)
        );
    }
    let store_names_found = store_names(&files);
    assert!(
        store_names_found.contains("analytics.db"),
        "the store-name reader did not see the planted `data/analytics.db` in {}: it read \
         {store_names_found:?}",
        shown(&store)
    );
    assert!(
        files
            .iter()
            .any(|(path, text)| path.ends_with("src/store.rs") && text.contains("SQLITE_OPEN_READ_WRITE")),
        "the read-write scan did not see the planted `SQLITE_OPEN_READ_WRITE` in {}",
        shown(&store)
    );
    let (named, separators) = named_files(&strip_comments(&read(&assets)));
    assert!(
        named.contains("index.html"),
        "the file-name reader did not see the planted `index.html`: it read {named:?}"
    );
    assert!(
        separators.is_empty(),
        "the file-name reader took a path segment for a separator: it read {separators:?}"
    );
    let fs = fs_functions(&files);
    assert!(
        fs.contains_key("write"),
        "the `std::fs` collector did not see the planted write: it read {fs:?}"
    );

    // 4. a member that is not Rust
    let parsed = members(&read(&manifest));
    assert_eq!(parsed, vec!["members/atlas-webby".to_string()], "the member reader read {parsed:?}");
    let strangers = non_rust_files(&planted.root.join("members/atlas-webby/src"));
    assert_eq!(
        strangers,
        vec![script.clone()],
        "the member scan did not find exactly the planted JavaScript"
    );

    // The other half: the same scanners on a clean tree stay silent, so a scanner that
    // fired on everything could not pass for a scanner that works.
    let clean = Planted::new("clean");
    clean.write(
        "src/app.rs",
        "fn draw(ui: &mut egui::Ui) {\n    // no HTML here: the note is a comment, not a hit\n    \
         ui.label(\"still no HTML\");\n}\n",
    );
    let clean_files = shipped(&clean.root.join("src"));
    assert!(
        web_hits(&clean_files).is_empty(),
        "the clean fixture was reported as web: {}",
        sentences(&web_hits(&clean_files))
    );
    assert!(
        write_hits(&clean_files).is_empty(),
        "the clean fixture was reported as a write: {}",
        sentences(&write_hits(&clean_files))
    );
    assert!(
        statement_hits(&clean_files).is_empty(),
        "the clean fixture was reported as a statement: {}",
        sentences(&statement_hits(&clean_files))
    );
    assert!(
        store_names(&clean_files).is_empty(),
        "the clean fixture was reported as naming a store file: {:?}",
        store_names(&clean_files)
    );
    assert!(
        forbidden_names(&tree_names("atlas-desktop v0.1.0\n└── egui v0.31.1\n")).is_empty(),
        "a clean closure was reported as a web frontend"
    );
    assert!(
        non_rust_files(&clean.root.join("src")).is_empty(),
        "a clean member was reported as carrying another language"
    );
}

/// A `mod tests;` in `src/data.rs` is `src/data/tests.rs` — not `src/tests.rs`.
///
/// This is the rule that decides what counts as shipped code, and it was wrong in a way
/// that stayed invisible for a while: the walk looked for the module file *beside* its
/// declaring file, which is right for `src/lib.rs` and wrong for every other module (`src/data.rs`
/// declares a child of `src/data/`, exactly as rustc resolves it). It only looked right
/// because `src/tests.rs` used to be a symlink to `src/data/tests.rs`, so the canonicalized
/// paths happened to match; when the symlink was removed, `fs::create_dir` inside a *test*
/// module was reported as "a directory this window made", and the test that says the window
/// writes one file failed for code that never ships.
///
/// Both directions are planted here, because over-excluding is the cheap way to make a
/// scanner green: a test module is not shipped, and an ordinary submodule still is.
#[test]
fn a_test_module_is_found_where_rustc_puts_it() {
    let planted = Planted::new("modules");
    planted.write("src/lib.rs", "pub mod data;\n\n#[cfg(test)]\nmod tests;\n");
    planted.write(
        "src/data.rs",
        "pub mod rows;\n\n/// Read one page.\npub fn page() {}\n\n#[cfg(test)]\nmod tests;\n",
    );
    planted.write("src/data/rows.rs", "pub fn rows() {}\n");
    // the two test modules: both write a file, and neither ships
    planted.write(
        "src/data/tests.rs",
        "fn t() {\n    std::fs::create_dir(\"data\").unwrap();\n}\n",
    );
    planted.write(
        "src/tests.rs",
        "fn t() {\n    std::fs::write(\"x\", b\"\").unwrap();\n}\n",
    );

    let files = shipped(&planted.root.join("src"));
    let shipped_names: Vec<String> = files
        .iter()
        .map(|(path, _)| {
            path.strip_prefix(planted.root.join("src"))
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    assert!(
        shipped_names.contains(&"data.rs".to_string()),
        "the module itself stopped shipping: {shipped_names:?}"
    );
    assert!(
        shipped_names.contains(&"data/rows.rs".to_string()),
        "an ordinary submodule was excluded — over-exclusion makes the rules green for the \
         wrong reason: {shipped_names:?}"
    );
    for excluded in ["data/tests.rs", "tests.rs"] {
        assert!(
            !shipped_names.contains(&excluded.to_string()),
            "`{excluded}` is declared under `#[cfg(test)]` and was read as shipped code: \
             {shipped_names:?}"
        );
    }
    assert!(
        write_hits(&files).is_empty(),
        "a test module's own writes were reported as the window's:\n  {}",
        sentences(&write_hits(&files))
    );
}
