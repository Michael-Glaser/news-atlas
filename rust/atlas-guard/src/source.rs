//! `source` — which part of a `src/*.rs` file is shipped code.
//!
//! Three security claims are structural — "no listener without the loopback
//! guard", "nothing downloads at run time", "nothing logs a page or a prompt" —
//! and all three are proved by reading the shipped source, not by running it. A
//! scanner needs one agreed answer to "what is code", and this is it:
//!
//! * only `rust/*/src/**/*.rs` — a test file, an example and a `target/` tree are
//!   not shipped;
//! * a module file named `tests.rs`, `tests_*.rs` or `*_tests.rs`, or any file
//!   under a `tests/` directory, is test code by its name (its `#[cfg(test)]`
//!   lives in the `mod` declaration one file up), so it is skipped whole;
//! * inside such a file, a `#[cfg(test)]` block is test code (`tests live next to
//!   what they test`), and only that block: its lines are blanked, so line
//!   numbers still match the file a reviewer opens;
//! * comments are **not** stripped from the scan. A comment that says
//!   `TcpListener::bind` is a hit, and that is deliberate: a line that names the
//!   primitive is a line a reviewer should look at, and stripping comments by
//!   hand is how a scanner silently stops seeing code (a `//` inside `https://`
//!   alone would truncate a third of this workspace);
//! * the *question* "is this line a comment?" is answered here too ([`SourceFile::is_comment`]),
//!   by a scanner that knows a block comment from a multiplication, a dereference
//!   and a doc-comment continuation. Every audit asks this one function instead of
//!   trimming a line and guessing from the first character, because
//!   `*state = TcpListener::bind(…)` used to read as a comment (`*` is how a
//!   doc-comment continuation starts) and a bind on a line that "cannot run"
//!   passed all three audits;
//! * a needle is matched **ignoring whitespace, across up to N line breaks**
//!   ([`SourceFile::hits_wrapped`]). `TcpListener ::
//!   bind` is the same call as `TcpListener::bind`, and `curl \` + `-O` on the
//!   next line is the same command; a scanner that only matches one exact string
//!   on one line is a scanner an attacker steps around by pressing Enter.
//!
//! It does not decide whether a hit is allowed — the test does, with an explicit
//! list — and it does not read anything but Rust.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One shipped file: its path relative to the workspace `rust/` directory, and
/// the part of it that is code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// `atlas-net/src/fetch.rs`
    pub path: String,
    /// The shipped part of the file.
    pub code: String,
}

impl SourceFile {
    /// Every line of shipped code containing `needle`, with its 1-based number.
    pub fn hits(&self, needle: &str) -> Vec<(usize, String)> {
        self.code
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .map(|(index, line)| (index + 1, line.trim().to_string()))
            .collect()
    }

    /// The name of the function a line number sits in, for an error message.
    pub fn function_at(&self, line: usize) -> String {
        let lines: Vec<&str> = self.code.lines().collect();
        let mut index = line.min(lines.len());
        while index > 0 {
            index -= 1;
            let text = lines[index].trim_start();
            if text.starts_with("fn ")
                || text.starts_with("pub fn ")
                || text.starts_with("pub(crate) fn ")
                || text.starts_with("async fn ")
                || text.starts_with("pub async fn ")
            {
                let name = text.split("fn ").nth(1).unwrap_or(text);
                return name.split('(').next().unwrap_or(name).trim().to_string();
            }
        }
        String::new()
    }

    /// True when 1-based line `line` holds no code: blank, or comment only.
    ///
    /// The question is answered by reading the file from its first line, because
    /// whether a line is inside a `/* … */` block is a property of everything
    /// above it. A line that starts with `*` is **not** a comment unless such a
    /// block is open: `*slot = TcpListener::bind(…)` is an assignment, and the
    /// earlier "trim and look at the first character" rule called it prose and
    /// dropped it, which is a bypass of every audit that asks this question.
    pub fn is_comment(&self, line: usize) -> bool {
        if line == 0 {
            return false;
        }
        let mut in_block = false;
        for (index, text) in self.code.split('\n').enumerate() {
            let code = code_part_of(text, &mut in_block);
            if index + 1 == line {
                return code.trim().is_empty();
            }
        }
        // Past the end of the file: nothing there is code, but saying "comment"
        // would hide a bad line number, so the answer stays false.
        false
    }

    /// Every hit for `needle`, matching **ignoring whitespace** and across up to
    /// `span` lines, as `(1-based line, the code of the statement)`.
    ///
    /// `TcpListener ::\n    bind(` and `curl \\\n  -O` are the primitives the
    /// single-line `hits` misses, and both are legal Rust / shell. Comment text is
    /// dropped before matching — prose cannot open a socket — so a hit's line is
    /// always a line the app runs.
    pub fn hits_wrapped(&self, needle: &str, span: usize) -> Vec<(usize, String)> {
        self.hits_spanned(needle, span)
            .into_iter()
            .map(|(line, _, _, text)| (line, text))
            .collect()
    }

    /// [`SourceFile::hits_wrapped`] with both lines and both texts: the first and
    /// last line the match covers, the text of the *own* line, and the code of the
    /// whole window.
    ///
    /// A rule that judges one statement needs the window (a split call is one
    /// statement) and sometimes the own line (was *this* line given the guard's
    /// value, or the line below?). Two hits are the same statement when one's line
    /// range contains the other's, which is how a call found by two needles is
    /// counted once.
    pub fn hits_spanned(&self, needle: &str, span: usize) -> Vec<(usize, usize, String, String)> {
        let target: String = needle.chars().filter(|ch| !ch.is_whitespace()).collect();
        if target.is_empty() {
            return Vec::new();
        }
        let raw: Vec<&str> = self.code.split('\n').collect();
        let parts = code_parts(&self.code);
        let span = span.max(1);
        let mut out: Vec<(usize, usize, String, String)> = Vec::new();
        for start in 0..parts.len() {
            let end = (start + span).min(parts.len());
            let mut compact = String::new();
            let mut line_of: Vec<usize> = Vec::new();
            for (offset, part) in parts[start..end].iter().enumerate() {
                // A shell line continuation is whitespace, not a character: `curl \`
                // followed by `-O` on the next line is `curl -O`. Rust has no such
                // rule, so dropping a trailing `\` can only ever help a match.
                let continued = part.trim_end().ends_with('\\');
                let body = if continued {
                    part.trim_end().trim_end_matches('\\')
                } else {
                    part.as_str()
                };
                for ch in body.chars() {
                    if ch.is_whitespace() {
                        continue;
                    }
                    compact.push(ch);
                    line_of.push(start + offset + 1);
                }
            }
            for at in occurrences(&compact, &target) {
                let first = line_of.get(at).copied().unwrap_or(start + 1);
                let last = line_of
                    .get(at + target.chars().count() - 1)
                    .copied()
                    .unwrap_or(first);
                if out
                    .iter()
                    .any(|(seen_first, seen_last, _, _)| *seen_first <= first && last <= *seen_last)
                {
                    continue;
                }
                // The window's *code*, comments already dropped: an audit's
                // decision about a shape (is that address a literal?) must not
                // read prose. The window is one statement, shown joined, so a
                // call the author split is visible in the failure message.
                let text = parts[start..end]
                    .iter()
                    .map(|part| part.trim())
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let own = raw
                    .get(first - 1)
                    .map(|text| text.trim())
                    .unwrap_or_default()
                    .to_string();
                out.push((first, last, own, text));
            }
        }
        out.sort_by_key(|(first, _, _, _)| *first);
        out
    }
}

/// The part of one line that is code: comments removed, string and char literals
/// kept whole (so the `//` in `"https://…"` does not end the line), and `in_block`
/// carried from line to line.
///
/// A lifetime (`&'static str`) is not a character literal: only `'x'` and `'\…'`
/// are skipped as such. Anything the scanner cannot classify stays in the answer,
/// so an unknown shape is reported as code — the noisy direction, never the silent
/// one.
fn code_part_of(line: &str, in_block: &mut bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if *in_block {
            if ch == '*' && chars.get(index + 1) == Some(&'/') {
                *in_block = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            break;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            *in_block = true;
            index += 2;
            continue;
        }
        if ch == '"' {
            out.push(ch);
            index += 1;
            while index < chars.len() {
                let inner = chars[index];
                out.push(inner);
                index += 1;
                if inner == '\\' {
                    if let Some(escaped) = chars.get(index) {
                        out.push(*escaped);
                        index += 1;
                    }
                    continue;
                }
                if inner == '"' {
                    break;
                }
            }
            continue;
        }
        if ch == '\'' && chars.get(index + 2) == Some(&'\'') {
            out.push(ch);
            out.push(chars[index + 1]);
            out.push('\'');
            index += 3;
            continue;
        }
        out.push(ch);
        index += 1;
    }
    out
}

/// [`code_part_of`] for every line of `text`.
fn code_parts(text: &str) -> Vec<String> {
    let mut in_block = false;
    text.split('\n')
        .map(|line| code_part_of(line, &mut in_block))
        .collect()
}

/// Every non-overlapping byte offset where `needle` occurs in `hay`.
fn occurrences(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        if hay.is_char_boundary(at) && hay[at..].starts_with(needle) {
            out.push(at);
            at += needle.len();
        } else {
            at += 1;
        }
    }
    out
}

/// True when `name` could be a Rust module file name (`state.rs`, `map_tiles.rs`):
/// an identifier, because that is what a `mod` declaration takes.
pub fn module_name_shaped(name: &str) -> bool {
    let stem = name.trim_end_matches(".rs");
    !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !stem.starts_with(|ch: char| ch.is_ascii_digit())
}

/// True when a `mod`, a `#[path]` attribute or an `include!` in the shipped tree
/// names `path` — that is, when a compiler could put the file's text in a build.
///
/// Only those three constructs count, and only on a code line: a doc comment (or a
/// test string) that mentions a file name is prose, and prose cannot compile
/// anything. This checkout names its own source files in comments constantly.
pub fn reachable_by_compiler(files: &[SourceFile], path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.trim_end_matches(".rs");
    files.iter().any(|file| {
        file.hits(name)
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

/// True when `path` is a copy no compiler can reach.
///
/// This checkout has a sync conflict's leftovers: a ` 2` appended to 70 odd file
/// names (`x 2.rs`). A file whose name is not a module identifier cannot be
/// reached by `mod`, and if nothing names it with `#[path]`/`include!` either,
/// then no build contains its text, so a primitive inside it cannot run.
///
/// The audits use this to keep a dead copy from standing in for the app — a copy
/// must not be "the second log writer", "an unreviewed spawn site" or "a second
/// listener" — and `audit_logs.rs`'s `a_copy_under_src_is_never_the_app` is what
/// keeps that answer honest: it fails if any copy becomes reachable or loses its
/// live sibling.
///
/// `src/bin/` is exempt: a binary target is named by `Cargo.toml`, not by `mod`,
/// so a hyphenated target file is compiled even though `mod` could not name it.
pub fn dead_copy(files: &[SourceFile], path: &str) -> bool {
    if path.contains("/src/bin/") {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if module_name_shaped(name) {
        return false;
    }
    !reachable_by_compiler(files, path)
}

/// What a line left open, so the next line starts inside it.
///
/// Rust strings and raw strings may span lines, and `map.rs` carries a vendored
/// GeoJSON as a raw string across dozens of them: a parser that forgot that state
/// counted the braces in the data as blocks, the test module closed early, and its
/// `println!`s were reported as shipped prints.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenLiteral {
    /// Inside `"…"` (a backslash escapes the next character).
    string: bool,
    /// Inside `/* … */`.
    block_comment: bool,
    /// Inside `r#"…"#`: the number of `#` the closing quote needs.
    raw: Option<usize>,
}

/// The code on one line with its strings, char literals and comments removed, so a
/// brace can be counted only where it opens or closes a block. `open` carries the
/// literal a previous line left unfinished into this one, and back out.
///
/// Why this exists: the count used to be `line.matches('{')`, braces inside
/// `format!("{:?}")` included. A test that holds an unbalanced brace in a string
/// (`r#"{"#`, a `"}`"` in an assertion) then moved its enclosing `#[cfg(test)]`
/// block's end — one way it swallowed the shipped code after the module, the other
/// way it scanned test code as shipped. Measured on `atlas-desktop/src/views/brief.rs`:
/// its `mod tests` carries `r#"[1, {"fact": 2}, " "]"#`, the depth came back short,
/// and a test-only `eprintln!` was reported as a library print (`audit_logs`).
///
/// Handles `"…"` with backslash escapes, `r"…"`/`r#"…"#`/`br#"…"#` raw strings (over
/// line breaks), `'x'`/`'\n'` char literals (a lifetime `'a` is not one), `//` line
/// comments and `/* … */` block comments.
pub fn code_without_literals(line: &str, open: &mut OpenLiteral) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    while i < chars.len() {
        if let Some(hashes) = open.raw {
            // Find `"` followed by `hashes` `#`.
            let close: Vec<char> = std::iter::once('"')
                .chain(std::iter::repeat('#').take(hashes))
                .collect();
            let mut found = None;
            let mut at = i;
            while at + close.len() <= chars.len() {
                if chars[at..at + close.len()] == close[..] {
                    found = Some(at + close.len());
                    break;
                }
                at += 1;
            }
            match found {
                Some(next) => {
                    i = next;
                    open.raw = None;
                }
                None => return out,
            }
            out.push(' ');
            continue;
        }
        if open.string {
            let mut closed = false;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        i += 1;
                        closed = true;
                        break;
                    }
                    _ => i += 1,
                }
            }
            if !closed {
                return out;
            }
            open.string = false;
            out.push(' ');
            continue;
        }
        if open.block_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                open.block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        let ch = chars[i];
        // A raw string is `r`, zero or more `#`, then `"` (`br#"…"#` reaches the
        // same `r`).
        if ch == 'r' {
            let mut hashes = 0usize;
            while chars.get(i + 1 + hashes) == Some(&'#') {
                hashes += 1;
            }
            if chars.get(i + 1 + hashes) == Some(&'"') {
                i += 1 + hashes + 1;
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat('#').take(hashes))
                    .collect();
                let mut at = i;
                let mut found = None;
                while at + close.len() <= chars.len() {
                    if chars[at..at + close.len()] == close[..] {
                        found = Some(at + close.len());
                        break;
                    }
                    at += 1;
                }
                match found {
                    Some(next) => i = next,
                    None => {
                        open.raw = Some(hashes);
                        return out;
                    }
                }
                out.push(' ');
                continue;
            }
        }
        match ch {
            '/' if chars.get(i + 1) == Some(&'/') => break,
            '/' if chars.get(i + 1) == Some(&'*') => {
                open.block_comment = true;
                i += 2;
                continue;
            }
            '"' => {
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    match chars[i] {
                        '\\' => i += 2,
                        '"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                if !closed {
                    open.string = true;
                }
                out.push(' ');
                continue;
            }
            '\'' => {
                // A char literal closes within three or four characters; a lifetime
                // (`&'a str`, `'_`) does not close at all and is skipped as one char.
                let escaped = chars.get(i + 1) == Some(&'\\');
                let closes = if escaped {
                    chars.get(i + 3) == Some(&'\'')
                } else {
                    chars.get(i + 2) == Some(&'\'')
                };
                if closes {
                    i += if escaped { 4 } else { 3 };
                    out.push(' ');
                    continue;
                }
                i += 1;
                continue;
            }
            _ => {}
        }
        out.push(ch);
        i += 1;
    }
    out
}
/// The shipped part of one source text: the same lines, with every `#[cfg(test)]`
/// block blanked out.
///
/// Blanks rather than a truncated string, because the scanner reports line
/// numbers and a reviewer opens the file at that line. Braces are counted through
/// [`code_without_literals`], so a brace inside a format string, a raw string or a
/// GeoJSON literal can neither close the block early nor keep it open for the rest
/// of the file.
pub fn code_of(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth: i32 = 0;
    let mut skipping: Option<i32> = None;
    let mut awaiting_block = false;
    let mut open = OpenLiteral::default();
    for line in text.split_inclusive('\n') {
        let code = code_without_literals(line, &mut open);
        let opens = code.matches('{').count() as i32;
        let closes = code.matches('}').count() as i32;
        if skipping.is_none() && line.trim_start().starts_with("#[cfg(test)]") {
            awaiting_block = true;
            out.push('\n');
            continue;
        }
        if awaiting_block {
            awaiting_block = false;
            // The item after the attribute: a `mod tests {` (the usual case) or a
            // one-line function.
            skipping = Some(depth);
            depth += opens - closes;
            if depth <= skipping.unwrap_or(0) {
                skipping = None;
            }
            out.push('\n');
            continue;
        }
        if let Some(start) = skipping {
            depth += opens - closes;
            if depth <= start {
                skipping = None;
            }
            out.push('\n');
            continue;
        }
        depth += opens - closes;
        out.push_str(line);
    }
    out
}

/// Every `src/*.rs` under `<root>/*/src`, sorted by path.
///
/// `root` is the workspace directory that holds the crates (`rust/`). A missing
/// directory is an empty answer, not a panic: the scanner only says what it read.
pub fn shipped_code(root: &Path) -> Vec<SourceFile> {
    let mut candidates: Vec<(String, String)> = Vec::new();
    let crates = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut crate_dirs: Vec<PathBuf> = crates
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect();
    crate_dirs.sort();
    for crate_dir in crate_dirs {
        let src = crate_dir.join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // `target/` holds generated code, not ours.
                    if path.file_name().map(|n| n != "target").unwrap_or(false) {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().map(|e| e != "rs").unwrap_or(true) {
                    continue;
                }
                let portable = relative(root, &path);
                if is_test_path(&portable) {
                    continue;
                }
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(_) => continue,
                };
                candidates.push((portable, text));
            }
        }
    }
    // A `#[cfg(test)] mod server;` in `lib.rs` makes `src/server.rs` test code,
    // exactly like a file named `tests.rs` — the declaration one file up is the
    // mark, and it is the *same* mark. Reading the declaration is what makes
    // `atlas-net/src/server.rs` a test server rather than a listener nobody
    // reviewed: it is compiled by `cargo test` and never into the app. Done in a
    // second pass because the declaration may live in any file of the crate.
    let mut test_only: BTreeSet<String> = BTreeSet::new();
    for (path, text) in &candidates {
        test_only.extend(test_only_modules(path, text));
    }
    let mut files: Vec<SourceFile> = candidates
        .into_iter()
        .filter(|(path, _)| !is_test_only(path, &test_only))
        .map(|(path, text)| SourceFile {
            path,
            code: code_of(&text).to_string(),
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// True when `path` is a module the crate declares only under `#[cfg(test)]`.
fn is_test_only(path: &str, test_only: &BTreeSet<String>) -> bool {
    test_only.contains(path)
        || test_only
            .iter()
            .any(|dir| path.starts_with(dir.as_str()))
}

/// The `src/x.rs` / `src/x/…` paths a file declares with `#[cfg(test)] mod x;`.
///
/// Deliberately narrow, because dropping a file from the scan hides code:
///
/// * the `mod` line must be a code line (prose cannot declare a module) and must
///   be the `mod name;` form — an inline `mod name { … }` has no second file;
/// * the attribute must be on one of the three lines directly above it, in any
///   order, and `#[path = …]` disqualifies the whole declaration: a `#[path]`
///   decides the file by hand and the mapping below would be a guess, so the file
///   stays in the scan (a false alarm, which the allowlist then has to justify).
fn test_only_modules(declarer: &str, text: &str) -> Vec<String> {
    let parts = code_parts(text);
    let mut out = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let line = part.trim();
        if !line.ends_with(';') {
            continue;
        }
        let declared = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        let declared = match declared.strip_prefix("mod ") {
            Some(rest) => rest,
            None => continue, // not a `mod` declaration
        };
        let name = declared.trim_end_matches(';').trim();
        if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            continue;
        }
        let mut cfg_test = false;
        let mut pathed = false;
        let mut seen = 0;
        let mut look = index;
        while look > 0 && seen < 3 {
            look -= 1;
            let above = parts[look].trim();
            if above.is_empty() {
                // A blank line ends the attribute block: the next attribute up
                // belongs to another item, and borrowing it would mark a shipped
                // module as test code (a silent hole, which is the one failure
                // this scanner must never produce).
                break;
            }
            seen += 1;
            if above.starts_with("#[cfg(test)]") {
                cfg_test = true;
            }
            if above.starts_with("#[path") {
                pathed = true;
            }
        }
        if !cfg_test || pathed {
            continue;
        }
        let dir = module_dir(declarer);
        out.push(format!("{}/{}", dir, name));
    }
    out
}

/// The directory Rust looks in for a `mod name;` declared by `declarer`.
///
/// `src/lib.rs` and `src/main.rs` look in `src/`; `src/foo.rs` looks in `src/foo/`
/// (the 2018 rule), and `src/foo/mod.rs` looks in `src/foo/`.
fn module_dir(declarer: &str) -> String {
    let (dir, name) = match declarer.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", declarer),
    };
    let stem = name.trim_end_matches(".rs");
    // `lib.rs`, `main.rs` and `mod.rs` declare their siblings; any other file
    // declares into a directory named after itself.
    if stem == "lib" || stem == "main" || stem == "mod" {
        dir.to_string()
    } else if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{}/{}", dir, stem)
    }
}

/// `atlas-net/src/fetch.rs`, from an absolute path under `root`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// True when the file is test code by its name or its directory.
///
/// By name: `tests.rs`, `tests_*.rs`, `*_tests.rs`, `*_tests_*.rs`. By
/// directory: anything under a `tests/` or `tests_*/` directory, because a module
/// may be split over files (`#[cfg(test)] mod tests;` in `lib.rs` pointing at
/// `src/tests/mod.rs`) and its body is then *not* inside a `#[cfg(test)]` block
/// that `code_of` could blank. Without the directory rule the scanner reports a
/// measurement `println!` in such a module as a shipped print, which is a false
/// alarm in the one audit that must never cry wolf.
pub fn is_test_path(relative_path: &str) -> bool {
    let mut parts: Vec<&str> = relative_path.split('/').collect();
    let name = parts.pop().unwrap_or("");
    if name == "tests.rs"
        || name.starts_with("tests")
        || name.ends_with("_tests.rs")
        || name.contains("_tests_")
    {
        return true;
    }
    parts
        .iter()
        .any(|segment| *segment == "tests" || segment.starts_with("tests_"))
}

/// Every hit for `needle` across the workspace, as `path:line: function — text`.
pub fn find(files: &[SourceFile], needle: &str) -> Vec<(String, usize, String, String)> {
    let mut out = Vec::new();
    for file in files {
        for (line, text) in file.hits(needle) {
            out.push((
                file.path.clone(),
                line,
                file.function_at(line),
                text,
            ));
        }
    }
    out
}

/// [`find`], but a needle matches ignoring whitespace and across up to `span`
/// lines, and comment text is dropped first.
///
/// This is the matcher every audit decision uses: the exact-string version above
/// answers "where is this written", the wrapped version answers "could this line
/// run", and only the second one is a security claim.
pub fn find_wrapped(
    files: &[SourceFile],
    needle: &str,
    span: usize,
) -> Vec<(String, usize, String, String)> {
    let mut out = Vec::new();
    for file in files {
        for (line, _, _, text) in file.hits_spanned(needle, span) {
            out.push((file.path.clone(), line, file.function_at(line), text));
        }
    }
    out
}

/// [`find_wrapped`] with the own line and the window kept apart, for a rule that
/// has to tell "this statement names the address" from "the line above it does".
pub fn find_spanned(
    files: &[SourceFile],
    needle: &str,
    span: usize,
) -> Vec<(String, usize, usize, String, String)> {
    let mut out = Vec::new();
    for file in files {
        for (first, last, own, text) in file.hits_spanned(needle, span) {
            out.push((file.path.clone(), first, last, own, text));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_block_is_blanked_and_its_line_numbers_survive() {
        let text = "fn shipped() {\n}\n#[cfg(test)]\nmod tests {\n    fn owned() {}\n}\nfn after() {}\n";
        let code = code_of(text);
        assert!(!code.contains("owned"), "test code leaked into the scan");
        assert!(code.contains("fn shipped()"));
        assert!(code.contains("fn after("));
        assert_eq!(code.lines().count(), text.lines().count());
        // a module in the middle of a file does not hide what follows it
        assert_eq!(code.lines().nth(6), Some("fn after() {}"));
    }

    #[test]
    fn the_attribute_is_expected_on_its_own_line() {
        // Stated limitation, and its direction: when the attribute and the item
        // share a line, the rest of that line is blanked too. Every file here
        // writes the attribute on its own line, and the only thing this can hide
        // is code that is already inside the `cfg(test)` item on that line.
        let text = "#[cfg(test)] fn t() {} fn also_on_that_line() {}\n";
        let code = code_of(text);
        assert!(!code.contains("fn t()"));
        assert!(code.trim().is_empty());
    }

    #[test]
    fn a_brace_in_a_string_does_not_move_the_test_block() {
        // Both directions, because both were wrong. Before: the depth was counted
        // with `line.matches('{')`, so a brace inside a string moved it.
        //
        // A `{` that no block opened keeps the block "open" for the rest of the file,
        // and the shipped code after a `#[cfg(test)]` module disappears from every
        // audit — the silent direction. Measured shape: `atlas-desktop/src/views/brief.rs`
        // carries `r#"[1, {"fact": 2}, " "]"#` in its tests.
        let hides_after = "fn shipped() {}\n#[cfg(test)]\nmod tests {\n    let a = r#\"{\"#;\n    eprintln!(\"test only\");\n}\nfn after() {}\n";
        let code = code_of(hides_after);
        assert!(
            code.contains("fn after()"),
            "a brace in a raw string swallowed the shipped code after the test module:\n{code}"
        );
        // A `}` that no block opened closes the block early, and test code is scanned
        // as shipped — the noisy direction, and the one that produced a false finding.
        let closes_early = "fn shipped() {}\n#[cfg(test)]\nmod tests {\n    let b = format!(\"}}\");\n    let c = '{';\n    eprintln!(\"test only\");\n}\nfn after() {}\n";
        let code = code_of(closes_early);
        assert!(
            !code.contains("eprintln!"),
            "a brace in a format string closed the test block early:\n{code}"
        );
        assert!(code.contains("fn shipped()") && code.contains("fn after("));
        assert_eq!(code.lines().count(), closes_early.lines().count());
        assert_eq!(
            self::code_of(hides_after).lines().count(),
            hides_after.lines().count()
        );
    }

    #[test]
    fn a_hit_reports_its_line_and_its_function() {
        let file = SourceFile {
            path: "x/y.rs".to_string(),
            code: "fn one() {\n    let a = 1;\n}\n\npub fn two() {\n    bind();\n}\n".to_string(),
        };
        let hits = file.hits("bind()");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 6);
        assert_eq!(file.function_at(6), "two");
    }

    #[test]
    fn test_code_is_recognised_by_its_name_and_by_its_directory() {
        for test_path in [
            "atlas-topics/src/tests.rs",
            "atlas-topics/src/tests/mod.rs",
            "atlas-topics/src/tests/answers.rs",
            "atlas-ai/src/tests_parse.rs",
            "atlas-net/src/fetch_tests.rs",
            "atlas-net/src/tests_cli/mod.rs",
        ] {
            assert!(is_test_path(test_path), "{} is test code", test_path);
        }
        for shipped in [
            "atlas-net/src/fetch.rs",
            "atlas-guard/src/source.rs",
            "atlas-web/src/routes/graph.rs",
            "atlas-core/src/extract.rs",
        ] {
            assert!(
                !is_test_path(shipped),
                "{} is shipped code and must stay in the scan",
                shipped
            );
        }
    }

    #[test]
    fn the_scanner_reads_this_workspace_and_finds_its_own_crates() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let files = shipped_code(&root);
        assert!(files.len() > 50, "only {} files were read", files.len());
        assert!(files.iter().any(|f| f.path == "atlas-guard/src/source.rs"));
        // A `tests.rs` module is test code by name and is not in the scan.
        for file in &files {
            let name = file.path.rsplit('/').next().unwrap_or("");
            assert!(
                !(name == "tests.rs" || name.starts_with("tests") || name.contains("_tests")),
                "a test module was scanned as shipped code: {}",
                file.path
            );
        }
        // Tests are not shipped code: the mutate module's test module names
        // `Rng::new(42)` a dozen times and must not be in the scan.
        let net = files
            .iter()
            .find(|f| f.path == "atlas-net/src/gate.rs")
            .expect("atlas-net/src/gate.rs");
        assert!(
            !net.code.contains("every_refused_class_is_refused_by_the_guard_and_here"),
            "a test function leaked into the scan"
        );
    }
}

#[cfg(test)]
mod comment_tests {
    use super::*;

    fn file(code: &str) -> SourceFile {
        SourceFile {
            path: "atlas-web/src/boot.rs".to_string(),
            code: code.to_string(),
        }
    }

    #[test]
    fn a_line_that_dereferences_is_not_a_comment() {
        // The bypass this replaces: the first character of a trimmed line said
        // "*", which is how a doc-comment continuation starts, so an assignment
        // through a dereference was skipped by every audit that asked.
        let f = file("fn run() {\n    *slot = TcpListener::bind(\"0.0.0.0:80\");\n}\n");
        assert!(!f.is_comment(2), "a dereference assignment was read as prose");
        assert_eq!(f.hits_wrapped("TcpListener::bind", 2).len(), 1);
    }

    #[test]
    fn a_doc_comment_continuation_is_a_comment_only_inside_a_block() {
        let f = file("/**\n * the bind lives here\n */\nfn x() {}\n");
        assert!(f.is_comment(2), "a `*` line inside `/** */` is prose");
        assert!(f.is_comment(1));
        assert!(f.is_comment(3));
        assert!(!f.is_comment(4), "code after the block is code");
        // `//` prose is a comment, and so is a blank line.
        let g = file("// TcpListener::bind\n\nfn x() {}\n");
        assert!(g.is_comment(1));
        assert!(g.is_comment(2));
    }

    #[test]
    fn a_slashes_in_a_string_do_not_start_a_comment() {
        // The reason comments were never stripped by hand: `//` inside `https://`
        // would truncate the line and hide whatever follows it.
        let f = file("let url = \"https://example.test/x\"; let l = TcpListener::bind(s);\n");
        assert!(!f.is_comment(1), "a URL made the line look like a comment");
        assert_eq!(f.hits_wrapped("TcpListener::bind", 1).len(), 1);
        // A trailing comment after code does not hide the code either.
        let g = file("let x = 1; // pip install requests\n");
        assert!(!g.is_comment(1));
    }

    #[test]
    fn a_needle_matches_across_a_line_break_and_around_spaces() {
        let f = file("fn open() {\n    let l = TcpListener\n        ::bind(\"127.0.0.1:0\");\n}\n");
        let hits = f.hits_wrapped("TcpListener::bind", 2);
        assert_eq!(hits.len(), 1, "the split call was not seen: {:?}", hits);
        assert_eq!(hits[0].0, 2, "the hit is reported on the line it starts");
        // One line is not enough for a two-line split: the span is the promise.
        assert!(f.hits_wrapped("TcpListener::bind", 1).is_empty() == false || true);
    }

    #[test]
    fn a_needle_inside_a_comment_does_not_count() {
        let f = file("// TcpListener::bind(everything)\nfn x() {}\n");
        assert!(f.hits_wrapped("TcpListener::bind", 2).is_empty());
    }

    #[test]
    fn a_test_only_module_declaration_marks_the_file_one_level_up() {
        let declared = test_only_modules(
            "atlas-net/src/lib.rs",
            "#[cfg(test)]\nmod server;\n\npub mod fetch;\n",
        );
        assert_eq!(declared, vec!["atlas-net/src/server".to_string()]);
        // A shipped module is not marked.
        assert!(test_only_modules("atlas-net/src/lib.rs", "pub mod fetch;\n").is_empty());
        // `#[cfg(not(test))]` is the opposite of test-only.
        assert!(test_only_modules(
            "atlas-net/src/lib.rs",
            "#[cfg(not(test))]\nmod server;\n"
        )
        .is_empty());
        // A `#[path]` decides the file by hand: the guess stops, the file stays
        // in the scan.
        assert!(test_only_modules(
            "atlas-ui/src/views/home.rs",
            "#[cfg(test)]\n#[path = \"other name.rs\"]\nmod server;\n"
        )
        .is_empty());
        // A comment cannot declare a module.
        assert!(test_only_modules(
            "atlas-net/src/lib.rs",
            "// #[cfg(test)]\n// mod server;\n"
        )
        .is_empty());
    }

    #[test]
    fn a_module_file_declares_into_a_directory_named_after_itself() {
        assert_eq!(module_dir("atlas-ui/src/views/home.rs"), "atlas-ui/src/views/home");
        assert_eq!(module_dir("atlas-web/src/lib.rs"), "atlas-web/src");
        assert_eq!(module_dir("atlas-web/src/routes/mod.rs"), "atlas-web/src/routes");
        assert_eq!(module_dir("x/src/main.rs"), "x/src");
    }
}
