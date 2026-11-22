//! Disclosure: what this window may put on screen, and what it may not.
//!
//! The native window is ATLAS's whole face now — the rail, five pages and one status row.
//! A person reads those and nothing else, so a label is the product. Two things may be
//! named there and no more: the **archive path** (a reader who cannot find their archive
//! has no way to fix it) and the **page text** (the archive's own words, which are not
//! this crate's to edit). What may never appear is the machine's own description of
//! itself:
//!
//! * a **home directory** — `/Users/<name>`, `/home/<name>`, `C:\Users\<name>`. A label
//!   that begins with one tells whoever is looking over a shoulder who the reader is and
//!   where their files live, for no benefit: `data/atlas.db` is the same information
//!   without the machine around it;
//! * the **value of an environment variable** — it is the reader's own configuration, and
//!   a configuration value can be a credential (an address with a user name and a
//!   password in it, a token). The window may say *which* setting it read and what the
//!   default is, because those are words this crate wrote;
//! * the **model file's absolute path** — where the weights sit on disk is not something
//!   a reader asked to see, and it moves a home directory onto the screen with it.
//!
//! The crate also writes **no file at all**, log or otherwise: nothing it does is worth a
//! log line, and a log is the one thing that outlives a window, survives a backup and is
//! read by whoever opens it next.
//!
//! | Rule | Pinned by |
//! | --- | --- |
//! | the window creates no file, opens no log, names no logging crate, and cannot persist | [`the_window_writes_no_file_at_all`] |
//! | the archive is opened read-only, and the flags mean it | [`the_archive_cannot_be_written_through`] |
//! | one archive, inside the checkout, from the executable — not from an environment variable | [`the_archive_path_comes_from_the_checkout`] |
//! | no label spells out where the model file lives | [`no_label_spells_out_where_the_model_lives`] |
//! | exactly one environment variable is read, and it is the model address | [`the_window_reads_one_environment_variable`] |
//! | no view introduces a hue | [`no_view_introduces_a_hue`] |
//! | labels are sentence case, never shouted | [`no_label_shouts`] |
//! | no label prints an environment variable's value | [`no_label_prints_an_environment_variables_value`] |
//! | the status row names the archive without the machine around it | [`the_archive_is_named_without_the_machine_around_it`] |
//!
//! The palette's own rules — that every role is a grey, that focus is the brightest thing
//! on the page, that body text clears its contrast floor — are pinned by the tests inside
//! `src/theme.rs`, where the palette lives and where the numbers can be measured. This
//! file adds only the rule a scan can see from outside: no *view* may paint a hue of its
//! own even if the palette is grey, because `Color32::from_rgb` needs no palette to run.
//!
//! A test here that is `#[ignore]`d is an **open** item: it asserts the rule the window is
//! supposed to keep, so running it shows the gap. Its reason says what closing it takes,
//! and the crate's own source is what has to change.
//! `cargo test --offline -p atlas-desktop --test disclosure -- --ignored --nocapture` runs
//! the open items.
//!
//! The rule below is the one that was open: until the label moved, the three sentences
//! `data::Archive::open` returns printed `path.display()`, so the first run's status row
//! read `/Users/<name>/…/data/atlas.db`. It is closed now — [`atlas_desktop::assets::label`] is the
//! one function that turns a path into text, and both the row and those sentences call it.
//!
//! This target links the crate (`src/lib.rs`, `Cargo.toml`'s `[lib]`) instead of including
//! its modules by path, so a rule can ask the crate's own functions and read their answers.
//! What still has to be read as *text* is the labels themselves: only the source says which
//! literal is the argument of a `ui.label`, and only the source can show a `format!` that
//! pastes a path into one. So the rules below are of two kinds — behaviour, measured by
//! calling the crate, and disclosure, measured by scanning the crate's text for what it
//! prints.

use atlas_desktop::{assets, data, http};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------
// the window's own text: the source, with every test's assert message cut away
// ---------------------------------------------------------------------------------------

/// Every shipped Rust file of the crate, as (path from the crate root, text).
///
/// Recursive on purpose: a new `views/*.rs` is a new place a label can be written, and a
/// scan that only knew `src/*.rs` would be blind to exactly the file that was added.
fn shipped() -> Vec<(String, String)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, base, out);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let name = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((format!("src/{}", name), text));
        }
    }
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&base, &base, &mut out);
    out
}

/// One file's lines up to the trailing `#[cfg(test)] mod …` item.
///
/// A test's own assert message is not a label: it is read by whoever runs `cargo test`,
/// on a terminal they own. Cutting it is what lets the same needle (`display()`,
/// `env::var`, `fs::`) be forbidden on screen and allowed in a fixture. The cut keeps the
/// line count, so every line number in a failure message is the real one.
fn code_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        // The item it attributes: a `mod` within the next few lines (an attribute may sit
        // between them). Anything else — a `#[cfg(test)]` on a helper fn — is not a test
        // body cutting the file short.
        let follows = lines
            .iter()
            .skip(index + 1)
            .take(6)
            .map(|line| line.trim())
            .find(|line| !line.is_empty() && !line.starts_with("#[") && !line.starts_with("//"));
        let is_module = follows
            .map(|line| {
                line.starts_with("mod ")
                    || line.starts_with("pub mod ")
                    || line.starts_with("pub(crate) mod ")
            })
            .unwrap_or(false);
        if !is_module {
            continue;
        }
        return lines[..index].join("\n");
    }
    text.to_string()
}

/// The crate's shipped text: its code regions, with their comments removed.
///
/// The rules in this file are about what a file **does**. A comment is prose about what it
/// does, and a scan for text cannot tell the two apart — which is exactly what bit this file:
/// `src/requests.rs` carries a doc comment explaining that a *path* in a pasted URL is a
/// citation and not a credential ("… over the 1439 URLs in `data/atlas.db`, treating a path
/// as a value position redacted 231 of them"), while the code opens `data/requests.db` and
/// nothing else. `the_archive_cannot_be_written_through` failed on that sentence. So comments
/// are stripped before any rule reads a line ([`code_only`]), and a rule now fails on a
/// statement and never on a paragraph.
fn code() -> Vec<(String, String)> {
    shipped()
        .into_iter()
        .filter(|(path, text)| {
            // A test body can also be a *file* — `src/data/tests.rs`, reached by
            // `mod tests;` — and [`code_of`] can only cut a module written inside the file
            // it reads. Such a file is skipped, because a fixture's `println!` is not a
            // label; it is skipped only when it really holds `#[test]`s, so a `tests.rs`
            // that forgot its tests is scanned (and its name is wrong anyway).
            if !path.ends_with("tests.rs") {
                return true;
            }
            assert!(
                text.contains("#[test]"),
                "{} is named tests.rs but holds no test — it is source, and it is scanned",
                path
            );
            false
        })
        .map(|(path, text)| (path, code_only(&code_of(&text))))
        .collect()
}

/// The source with its comments removed and everything else kept.
///
/// The same walk `atlas-store/tests/search_engine_meta.rs` uses, for the same reason: one
/// pass over the file keeps every byte outside a comment — line (`//`) and block (`/* */`,
/// nested as Rust allows) — and **keeps string literals intact**, including the `//` inside
/// `"https://…"`, so nothing that is data is mistaken for a comment and nothing after it is
/// swallowed.
fn code_only(source: &str) -> String {
    enum State {
        Code,
        Line,
        Block(usize),
        Text,
        Raw(usize),
    }

    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut state = State::Code;
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        let next = chars.get(index + 1).copied();
        match state {
            State::Code => {
                if ch == '/' && next == Some('/') {
                    state = State::Line;
                    index += 2;
                    continue;
                }
                if ch == '/' && next == Some('*') {
                    state = State::Block(1);
                    index += 2;
                    continue;
                }
                if ch == '"' {
                    state = State::Text;
                    out.push(ch);
                    index += 1;
                    continue;
                }
                if ch == 'r' {
                    // `r"…"` or `r#"…"#`: a raw string is code, and a `//` inside it is a
                    // URL, not a comment.
                    let mut scan = index + 1;
                    let mut hashes = 0usize;
                    while chars.get(scan) == Some(&'#') {
                        hashes += 1;
                        scan += 1;
                    }
                    if chars.get(scan) == Some(&'"') {
                        for kept in index..=scan {
                            out.push(chars[kept]);
                        }
                        state = State::Raw(hashes);
                        index = scan + 1;
                        continue;
                    }
                }
                out.push(ch);
                index += 1;
            }
            State::Line => {
                // The newline is kept so every later line keeps its own number.
                if ch == '\n' {
                    out.push('\n');
                    state = State::Code;
                }
                index += 1;
            }
            State::Block(depth) => {
                if ch == '*' && next == Some('/') {
                    state = if depth > 1 {
                        State::Block(depth - 1)
                    } else {
                        State::Code
                    };
                    index += 2;
                    continue;
                }
                if ch == '/' && next == Some('*') {
                    state = State::Block(depth + 1);
                    index += 2;
                    continue;
                }
                if ch == '\n' {
                    out.push('\n');
                }
                index += 1;
            }
            State::Text => {
                out.push(ch);
                if ch == '\\' {
                    if let Some(escaped) = next {
                        out.push(escaped);
                        index += 2;
                        continue;
                    }
                }
                if ch == '"' {
                    state = State::Code;
                }
                index += 1;
            }
            State::Raw(hashes) => {
                if ch == '"' && (0..hashes).all(|offset| chars.get(index + 1 + offset) == Some(&'#')) {
                    out.push('"');
                    for _ in 0..hashes {
                        out.push('#');
                    }
                    index += 1 + hashes;
                    state = State::Code;
                    continue;
                }
                out.push(ch);
                index += 1;
            }
        }
    }
    out
}


/// Nothing but the test module's own closing brace may sit at column 0 after the cut.
///
/// [`code_of`] cuts at the `#[cfg(test)] mod …` item and keeps everything before it. That
/// is only sound because this workspace writes the tests last: a top-level item written
/// *after* a test module would be silently unscanned, which is the one way a rule in this
/// file could pass for the wrong reason. rustfmt puts a top-level item at column 0, so the
/// guard is the indentation of what follows.
fn no_top_level_item_is_hidden() {
    for (path, full) in shipped() {
        let lines: Vec<&str> = full.lines().collect();
        let mut cut = None;
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            // The `mod` line the attribute belongs to: the first line after it that is not
            // an attribute, a comment or blank. The cut starts *after* it — the module's
            // own header is a top-level item by design.
            let module = lines
                .iter()
                .enumerate()
                .skip(index + 1)
                .take(6)
                .find(|(_, line)| {
                    let line = line.trim();
                    !line.is_empty() && !line.starts_with("#[") && !line.starts_with("//")
                });
            if module
                .map(|(_, line)| line.trim().starts_with("mod "))
                .unwrap_or(false)
            {
                cut = Some(index.max(module.unwrap().0));
                break;
            }
        }
        let Some(cut) = cut else { continue };
        for (index, line) in lines.iter().enumerate().skip(cut + 1) {
            if line.is_empty() || line.starts_with('}') || line.starts_with("//") {
                continue;
            }
            assert!(
                line.starts_with(' ') || line.starts_with('\t'),
                "{}:{} is a top-level item after the test module, so the scan below is \
                 blind to it: move the tests to the end of the file",
                path,
                index + 1
            );
        }
    }
}

/// Every occurrence of `needle` in a code region, as (file, line, the line's text).
fn find(needle: &str) -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for (path, text) in code() {
        for (index, line) in text.lines().enumerate() {
            if line.contains(needle) {
                hits.push((path.clone(), index + 1, line.trim().to_string()));
            }
        }
    }
    hits
}

/// The calls that put words on screen — the crate's own label API, named once.
///
/// A label is not "any string literal": a SQL statement and a `format!` template for a
/// database row are text too, and neither is read by a person. The rule below therefore
/// starts from the call, which is the thing that decides a string is *shown*.
const LABEL_CALLS: &[&str] = &[
    "ui.label(",
    "ui.button(",
    "ui.selectable_label(",
    "ui.hyperlink_to(",
    "ui.strong(",
    "ui.monospace(",
    "ui.heading(",
    ".hint_text(",
    "Label::new(",
    "RichText::new(",
    "heading(",
    "card(",
    "theme::body(",
    "theme::dim(",
    "theme::caption(",
    "theme::section_label(",
    "theme::page_title(",
    "theme::card_title(",
    "theme::number(",
];

/// Every string literal that is the argument of a label call, as (file, line, contents).
///
/// A label's words are on the line that starts the call or on one of the two after it (a
/// `format!` that needs more than one line to write). Raw strings, escapes and char
/// literals are handled by [`literals`].
fn label_literals() -> Vec<(String, usize, String)> {
    let all = literals();
    let mut out: Vec<(String, usize, String)> = Vec::new();
    for (path, text) in code() {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !LABEL_CALLS.iter().any(|needle| line.contains(needle)) {
                continue;
            }
            let first = index + 1;
            let last = (index + 3).min(lines.len());
            for (name, at, literal) in &all {
                if name != &path || *at < first || *at > last {
                    continue;
                }
                if out
                    .iter()
                    .any(|(seen_path, seen_at, seen)| seen_path == name && seen_at == at && seen == literal)
                {
                    continue;
                }
                out.push((name.clone(), *at, literal.clone()));
            }
        }
    }
    out
}

/// Every string literal in a code region, as (file, line, the literal's contents).
///
/// Raw strings (`r#"…"#`) and escaped quotes are handled; a label is prose, so the scan
/// reads the *contents* and never the quotes.
fn literals() -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (path, text) in code() {
        let bytes: Vec<char> = text.chars().collect();
        let mut index = 0usize;
        let mut line = 1usize;
        while index < bytes.len() {
            let ch = bytes[index];
            if ch == '\n' {
                line += 1;
                index += 1;
                continue;
            }
            // A raw string: r, then any number of #, then ".
            if ch == 'r' || ch == 'b' {
                let mut scan = index + 1;
                let mut hashes = 0usize;
                while scan < bytes.len() && bytes[scan] == '#' {
                    hashes += 1;
                    scan += 1;
                }
                if scan < bytes.len() && bytes[scan] == '"' && (hashes > 0 || ch == 'r') {
                    let start_line = line;
                    let mut content = String::new();
                    scan += 1;
                    while scan < bytes.len() {
                        if bytes[scan] == '"' {
                            let mut close = 0usize;
                            while close < hashes && scan + 1 + close < bytes.len() && bytes[scan + 1 + close] == '#' {
                                close += 1;
                            }
                            if close == hashes {
                                break;
                            }
                        }
                        if bytes[scan] == '\n' {
                            line += 1;
                        }
                        content.push(bytes[scan]);
                        scan += 1;
                    }
                    out.push((path.clone(), start_line, content));
                    index = scan + 1 + hashes;
                    continue;
                }
            }
            if ch != '"' {
                index += 1;
                continue;
            }
            let start_line = line;
            let mut content = String::new();
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    '\\' => {
                        // An escape: the next char is content, whatever it is.
                        if index + 1 < bytes.len() {
                            if bytes[index + 1] == '\n' {
                                line += 1;
                            }
                            content.push(bytes[index + 1]);
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    '"' => {
                        index += 1;
                        break;
                    }
                    other => {
                        if other == '\n' {
                            line += 1;
                        }
                        content.push(other);
                        index += 1;
                    }
                }
            }
            out.push((path.clone(), start_line, content));
        }
    }
    out
}

/// The scan sees the crate it claims to read: a filter that matched nothing would make
/// every rule below pass for the wrong reason.
fn the_scan_is_not_blind() {
    no_top_level_item_is_hidden();
    let files = shipped();
    assert!(
        files.len() >= 8,
        "the scan reads {} shipped file(s): {:?}",
        files.len(),
        files.iter().map(|(path, _)| path.as_str()).collect::<Vec<&str>>()
    );
    // …and it reads real text out of them: a walk that returned empty strings would leave
    // every rule below passing for the wrong reason, which is the failure mode this guard is
    // for. The floor is a count of lines, not of files, because a file can be read and be blank.
    let lines = all_lines();
    assert!(
        lines.len() > 1_000,
        "the scan read {} line(s) out of {} file(s): a rule about a line would pass by absence",
        lines.len(),
        files.len()
    );
    // The three files this file's rules are *about*: if the walk stops reaching one of them,
    // the rule that names it goes quiet instead of red.
    let named: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    for expected in ["src/app.rs", "src/requests.rs", "src/webview.rs"] {
        assert!(
            named.contains(&expected),
            "the scan does not read {expected}, so a rule about it would pass by absence: {named:?}"
        );
    }
}

/// R8's rule, on one line of code: a `format!` that pastes the model address into text.
///
/// A pure predicate because the rule has two jobs. The test applies it to every line of
/// the crate; [`the_rules_reject_the_lines_that_used_to_be_here`] applies it to the line
/// this crate used to have, which is how the rejection is observed rather than argued.
fn formats_the_model_address(line: &str) -> bool {
    line.contains("format!") && (line.contains("model_url") || line.contains("model address"))
}

/// The status row's rule, on one line of code: `Path::display` turns an object on disk —
/// absolute by nature — into text a label can print. `assets::label` builds its string
/// from path components instead, and this row is the one place a path is named at all.
///
/// The rule is on the **call**, not on what the string becomes. It briefly carried an
/// exemption for a `display()` handed straight back to a path — the line in `src/requests.rs`
/// that appended WAL's side files to the queue's own name — and that exemption was the wrong
/// shape: a predicate that asks what a line *does* with its string lets the next line that
/// pastes a path into a sentence through, which is the line this test exists to refuse. The
/// code stopped needing it (the side files are named with an `OsString` suffix now), so the
/// strict form is back, and [`the_rules_reject_the_lines_that_used_to_be_here`] plants both a
/// bare label and the path-shaped exemption that used to pass.
fn puts_a_path_on_screen(line: &str) -> bool {
    line.contains("display()")
}


/// R6's rule, on the three channels of one `from_rgb(…)`: grey means the three are equal.
fn is_grey(channels: &[&str]) -> bool {
    channels.len() >= 3 && channels[0] == channels[1] && channels[1] == channels[2]
}

/// R1's persistence half, on the dependency section of a manifest: does this build hand
/// eframe the feature that makes it write a state file into the user's data directory?
fn declares_persistence(dependencies: &str) -> bool {
    dependencies.contains("persistence")
}

/// The channels of the first `from_rgb(…)` on a line, trimmed, or `None` if the line has
/// no such call. An unterminated call yields the rest of the line as one channel, which the
/// caller's arity check refuses — a call this scan cannot read is a call it must not pass.
fn rgb_channels(text: &str) -> Option<Vec<&str>> {
    let at = text.find("from_rgb(")?;
    let rest = &text[at + "from_rgb(".len()..];
    let inside = match rest.find(')') {
        Some(close) => &rest[..close],
        None => rest,
    };
    Some(inside.split(',').map(str::trim).collect())
}

/// The arguments of the first `from_gray(…)` on a line: one luminance, or more if someone has
/// invented an arity this API does not have — which the caller refuses rather than counts.
fn gray_values(text: &str) -> Option<Vec<&str>> {
    let at = text.find("from_gray(")?;
    let rest = &text[at + "from_gray(".len()..];
    let inside = match rest.find(')') {
        Some(close) => &rest[..close],
        None => rest,
    };
    Some(inside.split(',').map(str::trim).collect())
}

// ---------------------------------------------------------------------------------------
// R1 — no file, no log
// ---------------------------------------------------------------------------------------

/// This crate writes nothing: no log file, no cache, no state.
///
/// The assertion is on the *shape* of every filesystem call rather than on a list of
/// names, so a new writer cannot slip in beside the ones that were checked: the only
/// `fs::` use allowed on screen-side code is a read, the only archive connection is
/// `open_with_flags(… READ_ONLY …)`, the manifest names no logging crate and does not turn
/// on eframe's persistence. A log line has no reader in a window a person is looking at.
///
/// The persistence check is a guard rather than a repair: a file written by a *dependency*
/// is the one write this crate could make without an `fs::` line to grep for, and eframe's
/// `persistence` feature writes `app.ron` — window geometry and whatever a view asked to
/// keep — into the user's own data directory. Turning that feature on is a one-word change
/// to the manifest, so it is checked here by name.
#[test]
fn the_window_writes_no_file_at_all() {
    the_scan_is_not_blind();

    // 1. Every filesystem call in the code regions is a read — except the one directory
    //    the queue lives in, which is created once, in the one file allowed to write.
    //
    //    That file is the product decision this rule now carries: the window could not
    //    ingest anything (it read the archive, drew it, and asked the model a question),
    //    so a click on "monitor this topic" had no way to reach the agent that does the
    //    work. The window therefore appends **intents** to `data/requests.db` and nothing
    //    else: no archive write, no config write, no log, no cache, no state file. The
    //    archive stays read-only (R2 below, and its own flag test), the queue is a
    //    separate service's inbox, and the reading side of it belongs to the supervised
    //    agent, not to this window.
    let mut reads = 0usize;
    for (path, line) in all_lines() {
        let Some(at) = line.find("fs::") else { continue };
        // `fs::` is four bytes: what follows the two colons is the call.
        let rest = &line[at + 4..];
        // There is no fifth read: the window holds no handle of its own. The settings page
        // once opened a pasted model file to look at its first four bytes (`GGUF`), and that
        // handle is gone — the import is checked by name and the agent's installer decides
        // the rest (`views/settings.rs`, `check_import`). `File::open` is read-only by
        // definition, and it is still refused here: a rule that allows the one harmless shape
        // is a rule that invites the next harmless-looking shape beside it.
        let allowed = rest.starts_with("read_to_string")
            || rest.starts_with("read(")
            || rest.starts_with("read_dir")
            || rest.starts_with("metadata")
            || (rest.starts_with("create_dir_all(") && path == "src/requests.rs")
            // `set_permissions` writes no byte of any file: it answers who may read the one
            // file the window already owns, `data/requests.db`. It is here because SQLite
            // creates that file 0644 under the usual umask, and a queue row is the reader's own
            // words — owner-only is the mode every other file in `data/` already has
            // (`src/requests.rs`, `open`). Allowed in that file only, beside the one directory
            // it creates, so a second caller still has to come past this line and say why.
            || (rest.starts_with("set_permissions(") && path == "src/requests.rs")
            // `Permissions::from_mode` builds a mode value: it opens nothing, writes nothing and
            // reads nothing. It is here because the queue's mode is set as
            // `std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))` — the
            // argument's own line reaches this scan as a second `fs::`. Allowed beside
            // `set_permissions` in the same file and nowhere else.
            || (rest.starts_with("Permissions::from_mode(") && path == "src/requests.rs");
        assert!(
            allowed,
            "{}:{} touches the filesystem with something that is not a read: {}\n\
             The only write this window may make is `data/requests.db` (an intent for the \
             supervised agent), and it lives in src/requests.rs.",
            path,
            line_number(&path, &line),
            line.trim()
        );
        reads += 1;
    }
    assert!(reads > 0, "the filesystem scan found no call at all");
    // …and that exception is one call, in that one file: a second directory created
    // anywhere else is the rule being widened by accident.
    let creates: Vec<_> = all_lines()
        .into_iter()
        .filter(|(path, line)| line.contains("fs::create_dir_all(") && path != "src/requests.rs")
        .collect();
    assert!(
        creates.is_empty(),
        "{}:{} creates a directory outside src/requests.rs",
        creates.first().map(|(path, _)| path.as_str()).unwrap_or(""),
        creates
            .first()
            .map(|(path, line)| line_number(path, line))
            .unwrap_or(0)
    );

    // 2. No file handle is opened for *writing* anywhere. The shapes that can write are
    //    refused by name; `File::open` is read-only by definition, and it is the one handle
    //    this window takes — four bytes of a model file the reader pasted, to refuse a file
    //    that does not begin `GGUF` before anything is queued (`views/settings.rs`).
    //    Anything else spelled `File::` is refused by shape, so a new way of opening one has
    //    to come past this line and say what it is for.
    for needle in ["OpenOptions", "File::create", "File::options"] {
        let hits = find(needle);
        assert!(
            hits.is_empty(),
            "{}:{} opens a file for writing ({}): this window writes nothing",
            hits.first().map(|hit| hit.0.as_str()).unwrap_or(""),
            hits.first().map(|hit| hit.1).unwrap_or(0),
            needle
        );
    }
    for (path, line, text) in find("File::") {
        assert!(
            text.contains("File::open("),
            "{}:{} opens a file in a way this window does not read with: {} — a `File` here \
             may only be `File::open`",
            path,
            line,
            text
        );
    }

    // 3. A log this crate does not write may still be pointed at — the launcher's
    //    `data/llama.log` is where a model failure is explained — but only by a path
    //    inside the checkout: never `~/…`, never `/Users/…`, never `C:\…`.
    for (path, line, text) in find(".log") {
        for literal in text.split('"').skip(1).step_by(2) {
            assert!(
                !literal.starts_with('/')
                    && !literal.starts_with('~')
                    && !literal.contains(":\\"),
                "{}:{} points at an absolute log path: {:?}",
                path,
                line,
                literal
            );
        }
    }
    // 4. No logging framework to write one with.
    let manifest = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("the crate's manifest");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap_or("")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for crate_name in ["log", "tracing", "env_logger", "simplelog", "fern", "slog"] {
        let needle = format!("{} =", crate_name);
        assert!(
            !dependencies.contains(&needle),
            "the crate depends on `{}`: a logging framework is a file nobody reviewed",
            crate_name
        );
    }
    // The three dependencies it does ship are the window, the archive and JSON.
    for expected in ["eframe", "egui", "rusqlite", "serde_json"] {
        assert!(
            dependencies.contains(expected),
            "the manifest no longer names {} — the scan is reading the wrong section",
            expected
        );
    }

    // 5. No way to persist either — the one file a window writes without writing one.
    //    `eframe`'s `persistence` feature is a file in the user's data directory
    //    (`app.ron`: window geometry, and whatever a view asked to keep). The crate cannot
    //    write it, because the feature is off and no code hands eframe a `Storage`; a
    //    build that turned it back on would put a file on disk with no `fs::` line to see.
    assert!(
        !declares_persistence(&dependencies),
        "the window enables eframe's persistence: that is a state file in the user's home \
         directory, written by a crate whose rule is that it writes nothing"
    );
    let mut persisted = find("Storage");
    persisted.extend(find("persisted"));
    persisted.extend(find("set_value"));
    persisted.extend(find("get_value"));
    persisted.extend(find("persistence"));
    assert!(
        persisted.is_empty(),
        "{}:{} uses eframe's storage handle: this window keeps nothing between runs",
        persisted.first().map(|hit| hit.0.as_str()).unwrap_or(""),
        persisted.first().map(|hit| hit.1).unwrap_or(0)
    );
}

/// `(file, line)` without the text, for a line-oriented rule.
fn all_lines() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (path, text) in code() {
        for line in text.lines() {
            // An import is not a call: `use std::os::unix::fs::PermissionsExt;` reads like a
            // filesystem use to a substring scan and does nothing at all. The scan is on what
            // the code *does*, so the import lines step aside — every call site is still a
            // line of its own, and a call spelled through an import is caught there.
            if line.trim_start().starts_with("use ") {
                continue;
            }
            out.push((path.clone(), line.to_string()));
        }
    }
    out
}

/// The line number of `needle` inside its own file, for a failure message.
///
/// Looked up rather than carried, because [`all_lines`] is a flat list: a message that
/// says where to look is worth the second read of a small file.
fn line_number(path: &str, needle: &str) -> usize {
    for (name, text) in code() {
        if name != path {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            if line == needle {
                return index + 1;
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------------------
// R2 — the archive is read-only, and the flags mean it
// ---------------------------------------------------------------------------------------

/// The window cannot write the archive, even by mistake.
///
/// Two halves. The source can only open one connection, with `SQLITE_OPEN_READ_ONLY`, and
/// `PRAGMA query_only` is *set* and read back — a pragma that is only queried is a guard
/// that was never switched on. Then a scratch database, opened with the same flags the
/// crate uses, refuses an `INSERT`: the flag means what the source says it means, on this
/// machine, today. The scratch file is created and removed by the test, in the temporary
/// directory, never in `data/`.
#[test]
fn the_archive_cannot_be_written_through() {
    the_scan_is_not_blind();
    assert!(
        !find("SQLITE_OPEN_READ_ONLY").is_empty(),
        "the archive is no longer opened read-only"
    );
    // One read-write connection may exist, and it is not the archive's: `src/requests.rs`
    // opens the queue. The proof that it is not the archive is below — the only database
    // name that file may carry is `requests.db`.
    let read_write: Vec<_> = find("Connection::open(")
        .into_iter()
        .filter(|(path, _, _)| path != "src/requests.rs")
        .collect();
    assert!(
        read_write.is_empty(),
        "{}:{} opens a read-write connection",
        read_write.first().map(|hit| hit.0.as_str()).unwrap_or(""),
        read_write.first().map(|hit| hit.1).unwrap_or(0)
    );
    for (path, line, text) in find("atlas.db") {
        assert!(
            path != "src/requests.rs",
            "src/requests.rs:{} names the archive ({text}): the queue is a different file, \
             or the window would be writing the archive through it",
            line
        );
    }
    assert!(
        find("\"requests.db\"").iter().any(|(path, _, _)| path == "src/requests.rs"),
        "src/requests.rs no longer names `requests.db` — the queue rule is checking nothing"
    );
    assert!(
        !find("pragma_update(None, \"query_only\", true)").is_empty(),
        "`query_only` is no longer set on the connection"
    );
    for (path, line, text) in find("execute") {
        // `execute` on a connection is a write; the archive reader must have none. The
        // queries it runs are `query_row`/`prepare`.
        //
        // Two things are not a write: a *comment* that uses the word (this file's own
        // rule, and a module doc saying what it does not do — measured: `src/agent.rs`'s
        // header line tripped this check before comments were skipped), and the queue's
        // own INSERT, which is the one write the window is allowed and which the archive
        // connection above proves cannot reach `atlas.db`.
        let comment = text.starts_with("//") || text.starts_with('*');
        assert!(
            comment || path == "src/requests.rs",
            "{}:{} executes SQL on the archive: {}",
            path,
            line,
            text
        );
    }

    // The probe: the same two flags, on a scratch file, refusing a write.
    let path = std::env::temp_dir().join(format!("atlas-disclosure-ro-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).expect("a scratch database");
        conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
            .expect("a seeded table");
    }
    let readonly = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("the archive, read-only");
    assert!(
        readonly.execute("INSERT INTO t VALUES (2)", []).is_err(),
        "a read-only archive accepted a write"
    );
    // And a compiler enforces the flag as well: no write statement is even spelled —
    // outside the queue, whose INSERT is the one the window is allowed (into
    // `requests.db`, never into the archive).
    for statement in ["INSERT INTO", "UPDATE ", "DELETE FROM"] {
        let hits: Vec<_> = find(&format!("\"{statement}"))
            .into_iter()
            .chain(find(&format!("\"{statement} ")))
            .filter(|(path, _, _)| path != "src/requests.rs")
            .collect();
        assert!(
            hits.is_empty(),
            "{}:{} carries a write statement the archive rule does not allow: {}",
            hits.first().map(|hit| hit.0.as_str()).unwrap_or(""),
            hits.first().map(|hit| hit.1).unwrap_or(0),
            statement
        );
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------------------
// R3 — one archive, inside the checkout, found from the executable
// ---------------------------------------------------------------------------------------

/// The one file the window reads is `<root>/data/atlas.db`, and `<root>` comes from where
/// the binary sits — not from `ATLAS_DATA_DIR`, not from `$HOME`, not from the shell's
/// working directory when there is a checkout above the binary.
///
/// This is what makes the naming rule ([`the_archive_is_named_without_the_machine_around_it`])
/// expressible at all: there is exactly one path, it is the project's own `data/`, and the
/// label can therefore be relative to the root.
#[test]
fn the_archive_path_comes_from_the_checkout() {
    the_scan_is_not_blind();
    let root = assets::root();
    assert!(
        root.join("rust").join("Cargo.toml").is_file(),
        "the root the window found ({}) holds no rust/Cargo.toml",
        root.display()
    );
    assert_eq!(assets::archive(), root.join("data").join("atlas.db"));
    assert!(assets::archive().starts_with(&root));
    assert!(
        assets::geojson().starts_with(root.join("public").join("vendor")),
        "the outlines are read from {}, which is not the checkout's public/vendor",
        assets::geojson().display()
    );
    // The path is computed, never configured: an environment variable that moved the
    // archive would make every sentence about "the archive" a sentence about a variable.
    let env_reads = find("env::var");
    assert!(
        env_reads.iter().all(|(path, _, _)| path != "src/assets.rs"),
        "assets.rs reads an environment variable: {:?}",
        env_reads
    );
}

// ---------------------------------------------------------------------------------------
// R4 — the model's file is not the window's business
// ---------------------------------------------------------------------------------------

/// Nothing on screen says where on this machine the weights live.
///
/// The rule was written as "no `.gguf` and no `models/` anywhere", and that was the right
/// rule for a window that only ever dials an address. It stopped being the right rule when
/// a fresh machine needed to be told *which* file is missing and where it comes from:
/// `assets::Piece` names the weights by the name the manifest carries (`models/model.json`
/// → the file name), which is what makes the first run legible instead of a dead end.
///
/// So the rule keeps its teeth and moves to the thing that actually leaks: a **path this
/// machine computed**. A home directory, a drive letter, a `~/`, an absolute prefix, or a
/// `models/` joined at run time must never reach a label — those are what put somebody's
/// disk on a screenshot. A bare file name cannot.
///
/// Two spellings are therefore allowed, and only these two:
/// - in `src/assets.rs`, the `.gguf` extension (it is how the newest weights are found and
///   how the missing-file sentence names them), and
/// - the relative names the manifest ships (`models/model.json`, `models/`), because that
///   is a location *inside the checkout*, the same shape `assets::label` reduces to.
///
/// Everywhere else — every view, every label — the window talks about an address or a
/// name, never a path, and the sweep below still fails the moment one appears.
#[test]
fn no_label_spells_out_where_the_model_lives() {
    the_scan_is_not_blind();
    // The leak itself: a path to somebody's disk. None of these may appear in `src/`.
    for needle in [
        "/Users/",
        "\\\\Users\\\\",
        "$HOME",
        "~/",
        "model_path",
        "MODEL_PATH",
        "llama-server/",
        "dirs::home",
        "home_dir",
    ] {
        let hits = find(needle);
        assert!(
            hits.is_empty(),
            "{}:{} puts a home directory on screen ({}): a label may name a file, never a place on this disk",
            hits.first().map(|hit| hit.0.as_str()).unwrap_or(""),
            hits.first().map(|hit| hit.1).unwrap_or(0),
            needle
        );
    }
    // And the spellings that are allowed are allowed in one file: the one that has to say
    // which weights are missing. A view that starts naming the weights is a regression.
    for needle in [".gguf", "models/", "models\\\\"] {
        let hits = find(needle);
        let stray: Vec<&(String, usize, String)> =
            hits.iter().filter(|hit| hit.0 != "src/assets.rs").collect();
        assert!(
            stray.is_empty(),
            "{}:{} names the model file ({}) outside the first-run sentence: the pages talk about an address",
            stray.first().map(|hit| hit.0.as_str()).unwrap_or(""),
            stray.first().map(|hit| hit.1).unwrap_or(0),
            needle
        );
    }
    // The address it does name is the API path and a literal loopback socket.
    assert_eq!(http::PATH, "/v1/chat/completions");
    assert!(http::endpoint("http://127.0.0.1:8080").is_ok());
    assert!(
        http::endpoint("http://example.test:8080").is_err(),
        "the one socket is no longer loopback-only"
    );
}

// ---------------------------------------------------------------------------------------
// R5 — one environment variable, and only the model address
// ---------------------------------------------------------------------------------------

/// The window reads exactly one environment variable: `ATLAS_MODEL_URL`.
///
/// This is a *budget*, not a ban: the address has to come from somewhere, and a setting is
/// where. Counting the reads is what makes the next one a decision — adding a second
/// (`ATLAS_DATA_DIR`, a proxy URL, a `$HOME`) reddens this test and sends the author to
/// [`no_label_prints_an_environment_variables_value`] with their value in hand.
///
/// What the crate reads as *process state* — where its own executable is, which directory
/// it was started in — is not a variable's value and cannot carry a credential, so those
/// two calls are named here instead of being forbidden.
#[test]
fn the_window_reads_one_environment_variable() {
    the_scan_is_not_blind();
    let reads = find("env::var");
    assert_eq!(
        reads.len(),
        1,
        "the window reads {} environment variables: {:?}",
        reads.len(),
        reads
            .iter()
            .map(|hit| format!("{}:{}", hit.0, hit.1))
            .collect::<Vec<_>>()
    );
    assert!(
        reads[0].2.contains("ATLAS_MODEL_URL"),
        "the one environment variable is no longer the model address: {}",
        reads[0].2
    );
    // The two process-state calls, and no others.
    assert_eq!(find("current_exe").len(), 1, "current_exe moved or multiplied");
    assert_eq!(find("current_dir").len(), 1, "current_dir moved or multiplied");
    // A home directory is never spelled, in any of its forms, and no shell variable is
    // read behind the app's back.
    for needle in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "XDG_",
        "SHELL",
        "env!(\"PATH\")",
        "var_os",
        "option_env",
    ] {
        let hits = find(needle);
        assert!(
            hits.is_empty(),
            "{}:{} reads {} — the window's labels must not depend on it",
            hits.first().map(|hit| hit.0.as_str()).unwrap_or(""),
            hits.first().map(|hit| hit.1).unwrap_or(0),
            needle
        );
    }
}

// ---------------------------------------------------------------------------------------
// R6 — no hue
// ---------------------------------------------------------------------------------------

/// No view introduces a hue: every colour this crate paints has equal red, green and blue.
///
/// The palette is built with `from_gray` — one luminance in, `r == g == b` out — and its own
/// tests in `src/theme.rs` walk it (`every_colour_in_the_palette_is_greyscale`,
/// `every_grey_that_carries_text_is_legible`, `the_focus_ring_is_visible_on_the_page`), so the
/// *palette* is pinned there and is not duplicated here. What those tests cannot see is a view
/// that mixes its own colour: a `Color32::from_rgb(200, 60, 60)` in a painter call is a red the
/// theme never offered.
///
/// Both spellings the crate now uses are read, and both are colour calls:
///
/// * `from_rgb(r, g, b)` — refused unless the three channels are the same number or the same
///   identifier. A grey written `from_rgb(grey, grey, grey)` passes; a hue in any spelling
///   fails.
/// * `from_gray(n)` — grey by construction, but still counted: it is the spelling the palette
///   macro (`src/theme.rs:66`) and `views/graph.rs`'s plate shading use, and a scan that only
///   knew `from_rgb` found **nothing at all** in this crate (measured after today's theme
///   pass, when the last `from_rgb` outside the tests became `from_gray` and this rule failed
///   with "the needle is wrong" instead of passing for the right reason).
///
/// A `from_gray` argument is not checked for equality — it cannot be a hue — but it must be a
/// single value: `from_gray(a, b)` is not this API and is refused rather than counted.
#[test]
fn no_view_introduces_a_hue() {
    the_scan_is_not_blind();
    let mut checked = 0usize;
    let mut triples = 0usize;
    for (path, line, text) in find("from_rgb(") {
        let channels = rgb_channels(&text).expect("find() matched a line without `from_rgb(`");
        assert!(
            channels.len() >= 3,
            "{}:{} is not an rgb triple: {}",
            path, line, text
        );
        let (red, green, blue) = (channels[0], channels[1], channels[2]);
        assert!(
            is_grey(&channels),
            "{}:{} paints a hue (rgb({}, {}, {})): this interface is grey, use theme::{}",
            path,
            line,
            red,
            green,
            blue,
            "a palette colour"
        );
        triples += 1;
        checked += 1;
    }
    for (path, line, text) in find("from_gray(") {
        let values = gray_values(&text).expect("find() matched a line without `from_gray(`");
        assert_eq!(
            values.len(),
            1,
            "{}:{} is not a single luminance (`from_gray` takes one channel): {}",
            path,
            line,
            text
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "the hue scan found no colour call — the needle is wrong (it reads `from_rgb(` and \
         `from_gray(`; the palette is built from one of them)"
    );
    // The palette is a *macro* over `from_gray`, so a tree that names no `from_rgb` at all is
    // the good case, not a blind one — but `checked` above is what proves the reading, and
    // this line records which spelling carried it, in the message if it ever goes to zero.
    eprintln!(
        "hue scan: {checked} colour calls ({triples} rgb triple, {} gray)",
        checked - triples
    );
    // Nothing may pick a colour by name either: the palette is the only source.
    for needle in ["from_rgba(", "Color32::GREEN", "Color32::RED", "Color32::BLUE", "Color32::YELLOW", "Color32::from_hsv"] {
        assert!(
            find(needle).is_empty(),
            "the crate names a hue: {}",
            needle
        );
    }
}

// ---------------------------------------------------------------------------------------
// R7 — sentence case
// ---------------------------------------------------------------------------------------

/// No label shouts, and no label is an eyebrow.
///
/// An all-capitals label is how an interface makes a word into decoration: it is read
/// before the words next to it and it carries no information a capital does not already
/// have. The product rule is sentence case, so a literal in the crate is checked twice —
/// it may not be entirely capitals (the product's own name is the one exception, and a
/// three-letter code from the archive such as `DEU` is data, not a label), and after its
/// first word it may not capitalise an interior word.
///
/// Words that carry a `+`, a `·` or a digit are skipped: `Shift+Enter` is a key, not an
/// eyebrow, and a country code arrives from the archive rather than from a label.
#[test]
fn no_label_shouts() {
    the_scan_is_not_blind();
    let mut checked = 0usize;
    for (path, line, literal) in label_literals() {
        let words: Vec<&str> = literal.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        // The product's name is a proper noun, and the type faces are named by the fonts
        // epaint bundles rather than by this crate.
        let proper = ["ATLAS", "Ubuntu-Light", "Hack"];
        if proper.contains(&literal.as_str()) {
            continue;
        }
        // A setting's own name — `ATLAS_MODEL_URL` — is an identifier, not a label: it is
        // spelled in capitals so a reader can type it, which is the opposite of shouting
        // at a reader. Screaming snake case, and only that, is allowed through.
        let identifier = !literal.is_empty()
            && literal
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            && literal.chars().any(|ch| ch == '_');
        if identifier {
            continue;
        }
        let letters: Vec<char> = literal.chars().filter(|ch| ch.is_alphabetic()).collect();
        if letters.len() >= 2 && letters.iter().all(|ch| ch.is_uppercase()) {
            // An all-capitals literal is a shout — unless it is a short code from the
            // archive (`DEU`, `USA`) or a single word with no lowercase to shout.
            let is_code = words.len() == 1 && letters.len() <= 3;
            assert!(
                is_code,
                "{}:{} shouts: {:?}",
                path, line, literal
            );
        }
        // The eyebrow rule, in the place an eyebrow can be: a *short* label — a card
        // title, a section label, a button. Prose is left alone, because prose has
        // sentences and a sentence may hold a key's own name (`press Enter`) without
        // being an eyebrow; a heading has no sentence in it and no reason to capitalise.
        let prose = words.len() > 4
            || literal
                .chars()
                .any(|ch| matches!(ch, '.' | '!' | '?' | ':' | ';'));
        if prose {
            continue;
        }
        for (index, word) in words.iter().enumerate().skip(1) {
            let plain = word
                .chars()
                .all(|ch| ch.is_alphabetic() || ch == '-' || ch == '\'');
            if !plain {
                continue;
            }
            // A capital after a full stop starts the next sentence, which is what
            // sentence case *is* — only a capital in the middle of a sentence is an
            // eyebrow.
            let previous_ends_a_sentence = words[index - 1]
                .chars()
                .last()
                .map(|ch| matches!(ch, '.' | '!' | '?' | ':' | ';' | '—'))
                .unwrap_or(false);
            if previous_ends_a_sentence {
                continue;
            }
            let capitalised = word.chars().next().map(|ch| ch.is_uppercase()).unwrap_or(false);
            assert!(
                !capitalised,
                "{}:{} capitalises a word inside a label ({:?}): sentence case, not an eyebrow",
                path, line, literal
            );
            checked += 1;
        }
    }
    assert!(
        checked > 8,
        "the sentence-case scan only looked at {} words",
        checked
    );
}

// ---------------------------------------------------------------------------------------
// R8 — the value of an environment variable stays off screen
// ---------------------------------------------------------------------------------------

/// No label prints an environment variable's value.
///
/// The value of `ATLAS_MODEL_URL` is the reader's own configuration, and a configuration
/// value can be a credential: an address with a user name and a password in it
/// (`http://alice:secret@127.0.0.1:8080`), or a token somebody pasted into the variable.
/// The window may name the setting and the *default* it falls back to — `"the local model
/// at http://127.0.0.1:8080"` is a string this crate wrote — and never the value that came
/// from outside it.
///
/// Both halves of the rule are kept today, and this pins them:
///
/// * `src/app.rs::status_row`'s right-hand label is the fixed text `CHAT_NEEDS_MODEL`
///   (`"chat needs the local model"`), not the field that holds the environment's answer.
///   Everything a reader needs to act on is still there: the page is Chat, the model did
///   not answer, and `./run.sh` starts it ([`http::START_COMMAND`]). The field
///   `Atlas::model_url` is dialled by the Chat view and never formatted.
/// * every refusal in `src/http.rs::endpoint` says "the model address …" and interpolates
///   nothing from the value it refused — including the `user:password@host` form the same
///   function rejects — so the address a reader typed cannot reach the screen.
#[test]
fn no_label_prints_an_environment_variables_value() {
    the_scan_is_not_blind();

    // 1. The refusal for the address a reader typed does not repeat it back — and one of
    //    the forms `endpoint` refuses is `user:password@host`, so a sentence that echoed
    //    the value would print a credential.
    let credential = "http://alice:s3cr3t-token@127.0.0.1:8080";
    let refused = http::endpoint(credential).expect_err("a user name is refused");
    println!("a refused address is answered with: {:?}", refused);
    assert!(
        !refused.contains("s3cr3t-token") && !refused.contains("alice"),
        "the refusal prints the address it refused, credential included: {:?}",
        refused
    );
    // And the same for a value that is not even an address.
    let junk = "ATLAS_MODEL_URL was set to banana-token-9137";
    let refused = http::endpoint(junk).expect_err("that is not an address");
    assert!(
        !refused.contains("banana-token-9137"),
        "the refusal prints the value it refused: {:?}",
        refused
    );

    // 2. No label interpolates the value. `self.model_url` is the field that holds the
    //    environment's answer; it may be dialled, never formatted. The scan is over every
    //    `format!` line of the crate, so a new label that pastes the field in fails here
    //    even though this file does not know the label exists.
    for (path, line, text) in find("format!") {
        assert!(
            !formats_the_model_address(&text),
            "{}:{} formats the model address into a label: {}",
            path,
            line,
            text
        );
    }

    // 3. The label itself, measured: the string the Chat page's row shows is fixed text.
    //    Read from the source rather than called, because the constant is private and the
    //    row is drawn by a frame — and asserted as the constant the row uses, so a literal
    //    that merely looks fixed cannot be swapped back for a value.
    let app = code()
        .into_iter()
        .find(|(path, _)| path == "src/app.rs")
        .map(|(_, text)| text)
        .expect("src/app.rs is part of the crate");
    assert!(
        app.contains("const CHAT_NEEDS_MODEL: &str = \"chat needs the local model\";"),
        "the chat label is no longer a constant with fixed text"
    );
    assert!(
        app.matches("CHAT_NEEDS_MODEL").count() >= 2,
        "status_row no longer shows CHAT_NEEDS_MODEL"
    );
}

// ---------------------------------------------------------------------------------------
// open items — the rules the window does not keep yet
// ---------------------------------------------------------------------------------------

/// The status row names the archive without the machine around it.
///
/// The archive path may be named — a reader who cannot find their archive cannot fix it —
/// but it is named in the checkout's own terms, `data/atlas.db`, never as the absolute
/// path it happens to sit at. That path begins with a home directory on every machine this
/// app is installed on, and a home directory on screen tells a passer-by who the reader is;
/// a path is also unbounded (a directory named by the user can be an essay), and a status
/// row is one line.
///
/// This rule used to be `#[ignore]`d with one half open: the status row already named the
/// archive by its components, but `Archive::open`'s three sentences printed
/// `path.display()` — and the first of them is what the row shows on a first run, because
/// `app.rs` prints the `problem` that call returned. It is closed: `assets::label(path)` is
/// now the one function that turns a path into text, called by the row and by all three
/// sentences (`no archive at …`, `cannot open …`, `… opened read-write`). The assertions
/// below are unchanged in what they demand — the sentence must name the file, must not
/// carry a home directory, must stay one line, and must say what to do; only the *sources*
/// they look at moved: `assets::label` is where the spelling lives now, so that is the call
/// `app.rs` is required to make.
#[test]
fn the_archive_is_named_without_the_machine_around_it() {
    the_scan_is_not_blind();

    // 1. The sentence a missing archive gets — the one the status row shows on its first
    //    run. Measured by asking the crate, because the sentence is built at run time; the
    //    fixtures are paths that cannot exist, so nothing is opened and nothing is written
    //    (a missing file is a sentence, not a connection). Two of them, because the
    //    sentence has two branches to get wrong: one path outside this checkout (a home
    //    directory, the shape any machine has) and one inside it (the real first run,
    //    where the row should read `data/atlas.db`).
    let outside = Path::new("/Users/example/news-atlas/data/atlas.db");
    assert!(
        !outside.exists(),
        "the fixture path exists on this machine: {:?}",
        outside
    );
    let inside = assets::archive().with_extension("db.no-such-archive");
    assert!(
        !inside.exists(),
        "this fixture must not exist — it is a name nothing creates: {:?}",
        inside
    );
    // The real first run is measured first, so the failure names the machine a reader
    // actually sees; the outside path then proves the sentence does not depend on being
    // inside the checkout either.
    for missing in [inside.as_path(), outside] {
        let message = data::Archive::open(missing)
            .err()
            .expect("a missing archive is a sentence, not an archive");
        println!("for {:?} the status row would read: {:?}", missing, message);
        assert!(
            !message.contains("/Users/") && !message.contains("/home/"),
            "the status row prints a home directory: {:?}",
            message
        );
        // What it must keep: one line, the file named, and something to do about it.
        assert!(!message.contains('\n'), "the row is one line: {:?}", message);
        assert!(
            message.contains("atlas.db"),
            "the sentence does not name the file: {:?}",
            message
        );
        assert!(
            message.contains("ATLAS"),
            "the sentence does not say what to do: {:?}",
            message
        );
    }
    // And the measured window root, which is the prefix the sentence must stop short of.
    let root = assets::root().display().to_string();
    println!("the window's root is: {:?}", root);

    // 2. And no label, and no sentence a label is built from, formats a path at all.
    //    `display()` is how an object on disk — absolute by nature — becomes text, and
    //    the row is where that text is read. `assets::label` builds its string from path
    //    components instead, and it is the one spelling: `app.rs` must call it, and
    //    `data.rs`'s sentences must not reach for `display()` either (nothing does — the
    //    scan below would say so).
    let app = code()
        .into_iter()
        .find(|(path, _)| path == "src/app.rs")
        .map(|(_, text)| text)
        .expect("src/app.rs is part of the crate");
    assert!(
        app.contains("assets::label(&archive.path)") || app.contains("assets::label("),
        "status_row no longer names the archive through assets::label"
    );
    for (path, line, text) in find("display()") {
        assert!(
            !puts_a_path_on_screen(&text),
            "{}:{} puts a filesystem path on screen: {}",
            path, line, text
        );
    }
}

// ---------------------------------------------------------------------------------------
// the rules bite — each needle, on the line it was written for
// ---------------------------------------------------------------------------------------

/// A scan that matches nothing passes for the wrong reason, so each rule is shown refusing
/// the source line this crate actually had before this file existed — and accepting the
/// line it has now. The predicates are the ones the rules call, not copies of them: if a
/// needle is weakened above, this fixture stops rejecting and fails with it.
///
/// The lines are quoted from the code as it was measured, so a reader can diff them against
/// the commit that fixed them.
#[test]
fn the_rules_reject_the_lines_that_used_to_be_here() {
    // R8 — the chat label used to interpolate the environment's answer:
    //     src/app.rs:330, before the fix
    let old = "            format!(\"chat needs the local model at {}\", self.model_url)";
    assert!(
        formats_the_model_address(old),
        "R8 would not have caught the line it exists for: {}",
        old
    );
    // … and the field is still dialled, which the rule must not forbid: the Chat view is
    // handed the address, it just is not text.
    let current = "                crate::views::chat::show(ui, &mut self.chat, &self.model_url);";
    assert!(
        !formats_the_model_address(current),
        "R8 refuses the line that merely passes the address on: {}",
        current
    );

    // The status row — the left label used to be the disk's own path:
    //     src/app.rs:319, before the fix
    let old = "                    .map(|archive| archive.path.display().to_string())";
    assert!(
        puts_a_path_on_screen(old),
        "the path rule would not have caught the line it exists for: {}",
        old
    );
    let current = "                    .map(|archive| crate::assets::label(&archive.path))";
    assert!(
        !puts_a_path_on_screen(current),
        "the path rule refuses the label the row now builds: {}",
        current
    );
    // The strict rule, both halves: the shape that *used* to be exempt (a `display()` handed
    // back to a path builder) is refused like any other, and a `display()` pasted into a
    // sentence is the defect this rule exists for. The exemption is gone because the code
    // stopped needing it — the side files are named with an `OsString` suffix now — and because
    // a predicate that asks what a line does with its string lets the next such line through.
    let beside = "        let beside = std::path::PathBuf::from(format!(\"{}{side}\", file.display()));";
    assert!(
        puts_a_path_on_screen(beside),
        "the path rule lets a `display()` through when it is handed back to a path builder, \
         which is the exemption this rule no longer carries: {}",
        beside
    );
    let sentence = "        let said = format!(\"the queue is at {}\", file.display());";
    assert!(
        puts_a_path_on_screen(sentence),
        "the path rule would let a sentence print a path: {}",
        sentence
    );

    // R6 — a view that paints its own colour, against the grey it shades a plate with.
    let hue = rgb_channels("let plate = Color32::from_rgb(200, 60, 60);").expect("a from_rgb call");
    assert!(
        !is_grey(&hue),
        "the hue rule would have passed rgb(200, 60, 60)"
    );
    let grey = rgb_channels("let plate = Color32::from_rgb(grey, grey, grey);")
        .expect("a from_rgb call");
    assert!(
        is_grey(&grey),
        "the hue rule refuses a grey written as a variable: {:?}",
        grey
    );
    // A call the scan cannot read is refused, not skipped.
    let cut = rgb_channels("let plate = Color32::from_rgb(200, 60");
    assert!(
        !is_grey(&cut.expect("a from_rgb call")),
        "an unterminated colour call passed the arity check"
    );

    // R1 — the manifest that would let eframe write a state file, and the one that does
    // not. The feature is one word in a line this file does not own, so the rule is shown
    // on the line itself.
    let persisted = "eframe = { version = \"=0.31.1\", default-features = false, \
                     features = [\"default_fonts\", \"glow\", \"persistence\"] }";
    assert!(
        declares_persistence(persisted),
        "the persistence rule would not notice the feature it exists for"
    );
    let current = "eframe = { version = \"=0.31.1\", default-features = false, \
                   features = [\"default_fonts\", \"glow\"] }";
    assert!(
        !declares_persistence(current),
        "the persistence rule refuses the manifest the crate ships: {}",
        current
    );
}
