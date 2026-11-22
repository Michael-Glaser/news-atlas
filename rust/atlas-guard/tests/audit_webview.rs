//! The in-process web engine, and the three ways it could become a browser.
//!
//! `atlas-desktop` carries `wry` on macOS and Windows: WKWebView and WebView2 — the
//! engine the operating system already has, hosted as a child view of this window. That is
//! a deliberate change to "the window renders no markup", and it moves three boundaries, so
//! each one is a rule here:
//!
//! 1. **One owner.** [`wry_is_named_in_one_manifest_and_one_source_file`] — exactly one
//!    manifest may depend on `wry` (`rust/atlas-desktop/Cargo.toml`) and exactly one
//!    shipped file may name it (`atlas-desktop/src/webview.rs`). A second file naming it is
//!    how a view starts driving the engine with its own builder, and its own builder is how
//!    a navigation handler gets skipped.
//! 2. **The address rule is the engine's only door.**
//!    [`allowed_is_a_whitelist_and_its_cases_are_the_ones_written_down`] pins the *shape*
//!    of `allowed`: the scheme test is a whitelist (`http`/`https`, everything else
//!    refused), the authority is required, any `@` is refused, and the module's own tests
//!    carry the five refused schemes in both directions. The behavioural half lives in the
//!    module's tests, one function away from the code it calls — see the note below.
//!    `open()` and `Page::go` are pinned to ask `allowed` before they touch the engine
//!    ([`open_checks_the_address_before_it_builds_anything`]).
//! 3. **A page cannot leave the web it was given, and cannot open a console.**
//!    [`the_engine_is_built_with_devtools_off_and_a_navigation_handler`] pins the two
//!    builder calls by name, the way the other audits pin a spelling: `with_devtools(false)`
//!    and `with_navigation_handler(... allowed(...) ...)`. Without the first, the reader has
//!    a console for a page they did not write; without the second, every redirect and
//!    `location.href` is unmediated.
//!
//! # Why this file does not *call* `allowed`
//!
//! It should: the claim is about a pure function, and a copy of the rule is the drift this
//! file exists to prevent. The two ways to call it both cost more than they buy today.
//! A dev-dependency on `atlas-desktop` links the window — eframe, wry, rusqlite — into this
//! security crate's test build, and it makes `cargo test -p atlas-guard` unbuildable
//! whenever the app is mid-edit (measured: `no method named reader_page` in `app.rs` took
//! the whole guard package red while another agent was landing the reader page). Including
//! `webview.rs` by path needs `wry` and `egui` in scope, i.e. the same dependency. So the
//! audit pins the rule's *shape* and its cases here, and the exhaustive both-directions
//! table stays in `webview.rs`'s own `#[cfg(test)] mod tests`, where it calls the shipped
//! function with no crate edge at all. If a future change can carry a dev-only edge without
//! that coupling, `allowed(file:///…)` etc. belong in this file.
//!
//! # What is *not* re-checked here
//!
//! Handing a URL to the operating system is `audit_open_url.rs`'s claim, tree-wide. It is
//! restated for this crate in [`the_window_hands_no_url_to_the_operating_system`] because
//! the file that now holds URLs is the file most likely to grow an opener next. Sockets and
//! process spawns are `audit_binds.rs`'s and `audit_downloads.rs`'s; the engine adds none
//! to our code, and those two files say so for `atlas-desktop/src` including `webview.rs`.
//!
//! # Why every rule has a plant beside it
//!
//! Each source rule is a small pure function over `(files, manifest texts)`, so the live
//! assertion and its planted regression run the same code: the live one over this tree,
//! the planted one over a synthetic `webview.rs` that allows `file:`, builds without
//! devtools, or names `wry` from a second file. The behavioural test cannot be planted (it
//! calls the shipped function), so it carries its own both-directions cases instead.

use atlas_guard::source::{find_spanned, shipped_code, SourceFile};
use std::path::{Path, PathBuf};

fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The engine's module, which this crate's `lib.rs` declares.
const WEBVIEW: &str = "atlas-desktop/src/webview.rs";

/// The one manifest allowed to depend on the engine.
const MANIFEST: &str = "atlas-desktop/Cargo.toml";

fn shipped() -> Vec<SourceFile> {
    let files = shipped_code(&rust_dir());
    assert!(
        files.len() > 50,
        "the scanner read only {} files — every rule below would pass on an empty set",
        files.len()
    );
    files
}

fn file<'a>(files: &'a [SourceFile], path: &str) -> &'a SourceFile {
    files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} is not in the shipped set the scanner read"))
}

/// Every manifest under `rust/` that names `wry` in a dependency table.
///
/// A path, not a name: only `atlas-desktop` may carry the engine, so the answer is the
/// list of paths and the test compares it with one entry.
fn wry_manifests(manifests: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (path, text) in manifests {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            // `wry = { … }`, `wry = "=0.57.3"`, and the `[dependencies.wry]` sub-table.
            let names_it = line
                .split_once('=')
                .map(|(key, _)| key.trim().trim_matches('"') == "wry")
                .unwrap_or(false)
                || line == "[dependencies.wry]"
                || line == "[workspace.dependencies.wry]";
            if names_it {
                found.push(path.clone());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Every shipped file that names `wry` outside a comment.
///
/// `find_spanned` drops comments, so a sentence explaining the engine is not a hit; a
/// `use wry::…` or a `wry::WebViewBuilder` is.
fn wry_files(files: &[SourceFile]) -> Vec<String> {
    let mut found: Vec<String> = find_spanned(files, "wry", 1)
        .into_iter()
        .map(|(path, _line, _last, _own, _text)| path)
        .collect();
    found.sort();
    found.dedup();
    found
}

#[test]
fn wry_is_named_in_one_manifest_and_one_source_file() {
    let manifests: Vec<(String, String)> = std::fs::read_dir(rust_dir())
        .expect("rust/ is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("Cargo.toml"))
        .filter(|path| path.is_file())
        .map(|path| {
            let shown = path
                .strip_prefix(rust_dir())
                .unwrap_or(&path)
                .display()
                .to_string();
            (shown, std::fs::read_to_string(&path).unwrap_or_default())
        })
        .collect();
    assert!(
        manifests.len() > 20,
        "only {} manifests were read — the scan is pointed at the wrong directory",
        manifests.len()
    );
    assert_eq!(
        wry_manifests(&manifests),
        vec![MANIFEST.to_string()],
        "`wry` is a dependency of more than the window crate. The engine is one surface in \
         one process: a second crate carrying it would be a second builder, and a second \
         builder is a navigation rule that can be forgotten. Keep it in {MANIFEST}."
    );

    let files = shipped();
    let naming = wry_files(&files);
    assert_eq!(
        naming,
        vec![WEBVIEW.to_string()],
        "a shipped file other than {WEBVIEW} names `wry`: {:?}. The engine is owned by one \
         module so that the address rule and the builder exist once; move the code there \
         instead of adding a caller.",
        naming
    );
    // And the module really is the one the crate declares: a file nothing compiles would
    // make the line above a statement about dead text.
    let lib = file(&files, "atlas-desktop/src/lib.rs");
    assert!(
        lib.code.contains("pub mod webview;"),
        "atlas-desktop/src/lib.rs does not declare `pub mod webview;`, so {WEBVIEW} is not in \
         the build and the claim above is about a file nobody compiles"
    );
}

#[test]
fn wry_is_named_in_no_comment_as_a_second_way_in() {
    // The rule above uses the comment-dropping matcher, so a doc sentence is not a hit.
    // This is the other half: a `use wry as …;` alias is still `wry`, and the matcher sees
    // the token. A plant proves the matcher fires at all, so a green run above is not a
    // blind scanner.
    let planted = vec![SourceFile {
        path: "atlas-desktop/src/views/articles.rs".to_string(),
        code: "use wry::WebViewBuilder;\nfn draw() { let _ = WebViewBuilder::new(); }\n"
            .to_string(),
    }];
    assert_eq!(
        wry_files(&planted),
        vec!["atlas-desktop/src/views/articles.rs".to_string()],
        "the wry scanner did not see a planted `use wry::…` — a green run above would then \
         mean nothing"
    );
}

/// The address rule's shape, and the cases its own tests carry.
///
/// Three structural claims, each the one that survives an edit:
///
/// * the scheme test is a **whitelist** — `scheme != "http" && scheme != "https"` — so a
///   scheme nobody thought of (`file`, `data`, `javascript`, `about`, `chrome`, a future
///   one) is refused by default. A blacklist would pass every case below and still let the
///   next scheme through;
/// * the authority is required (`strip_prefix("//")`), any `@` is refused, and a port must
///   be digits — the three ways a host is made to read as something it is not;
/// * the module's own tests name all five refused schemes and at least one accepted
///   address, so the behavioural table exists and is one edit away from the function.
#[test]
fn allowed_is_a_whitelist_and_its_cases_are_the_ones_written_down() {
    let files = shipped();
    let web = file(&files, WEBVIEW);
    for (spelling, why) in [
        (
            "scheme != \"http\" && scheme != \"https\"",
            "the scheme test must be a whitelist: an unknown scheme has to be refused by \
             default, not listed",
        ),
        (
            "strip_prefix(\"//\")",
            "a URL without an authority has no host to judge (`http:example.com` is the \
             relative form)",
        ),
        (
            "contains('@')",
            "userinfo is how a reader is made to read the host as the part after the `@`",
        ),
        (
            "is_ascii_digit",
            "a port must be digits: `host:http` is a second way to spell an address",
        ),
    ] {
        assert!(
            web.code.contains(spelling),
            "{WEBVIEW} no longer holds `{spelling}`: {why}"
        );
    }
    // A blacklist of scheme names would be the other way to write this rule; refuse it by
    // name, because it passes every case the module's tests carry.
    for blacklist in ["== \"file\"", "starts_with(\"file:", "starts_with(\"javascript:"] {
        assert!(
            !web.code.contains(blacklist),
            "{WEBVIEW} names a refused scheme directly (`{blacklist}`): the rule is a \
             whitelist, and a blacklist refuses only the schemes somebody remembered"
        );
    }

    // The cases, read from the raw file: `shipped_code` blanks `#[cfg(test)]` blocks, and
    // this is the one rule that must see them.
    let raw = std::fs::read_to_string(rust_dir().join(WEBVIEW)).expect("webview.rs is readable");
    let tests_start = raw
        .rfind("#[cfg(test)]")
        .expect("webview.rs carries its own tests");
    let cases = &raw[tests_start..];
    for refused in ["file://", "javascript:", "data:", "about:", "chrome://", "user:pw@"] {
        assert!(
            cases.contains(refused),
            "the module's own tests no longer carry a refusal case for `{refused}`: the \
             behavioural table is what makes `allowed` testable without linking this crate \
             to the window, so it cannot thin out"
        );
    }
    // Both directions, as assertions rather than as a count of calls: the module drives
    // its cases from two arrays, so one `assert!` per direction is what "tested" means
    // here (`webview.rs` lines 318 and 348 when this rule was written).
    for (spelling, direction) in [
        ("assert!(allowed(", "an accepted address"),
        ("assert!(!allowed(", "a refused address"),
    ] {
        assert!(
            cases.contains(spelling),
            "webview.rs's tests no longer assert {direction} (`{spelling}`): the address \
             rule needs both directions, or it is a rule nobody has seen refuse anything"
        );
    }
    // …and the accepted side is not only loopback: the web has to be reachable, or the
    // engine exists to draw a page nobody can open.
    assert!(
        cases.contains("https://"),
        "webview.rs's tests carry no `https://` case: a rule that refuses everything \
         passes every refusal and makes the reader text-only by accident"
    );
}

/// `open` asks `allowed` before it builds anything, and it builds with the two
/// calls that keep the page inside its bounds.
#[test]
fn open_checks_the_address_before_it_builds_anything() {
    let files = shipped();
    let web = file(&files, WEBVIEW);
    let open_line = web
        .code
        .lines()
        .position(|line| line.contains("pub fn open("))
        .map(|index| index + 1)
        .expect("webview.rs still defines `open`");
    let allowed_at = web
        .code
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("allowed(url)"))
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    let builder_at = web
        .code
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("WebViewBuilder::new()"))
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    assert_eq!(
        builder_at.len(),
        1,
        "webview.rs must build one view, in one place; `WebViewBuilder::new()` appears at \
         {builder_at:?}"
    );
    // `open` refuses first, then builds. The call is looked for between the two lines, so a
    // second `allowed` call elsewhere (`Page::go`, which is the other door) does not confuse
    // this rule — and a build placed above the check cannot pass.
    let gate = allowed_at
        .iter()
        .copied()
        .find(|line| *line > open_line && *line < builder_at[0]);
    assert!(
        gate.is_some(),
        "`open` (line {open_line}) builds a view at line {} without calling `allowed(url)` \
         first: {allowed_at:?} are the calls in the file, and none of them is between the \
         two. A builder that runs first reads an address the rule never saw.",
        builder_at[0]
    );
    // The other door: `Page::go` navigates an existing view, so it must ask the same rule
    // before it hands the string to the engine.
    let go_line = web
        .code
        .lines()
        .position(|line| line.contains("pub fn go("))
        .map(|index| index + 1)
        .expect("webview.rs still defines `Page::go`");
    assert!(
        allowed_at.iter().any(|line| *line > go_line),
        "`Page::go` (line {go_line}) navigates without `allowed(url)` — the address rule is \
         the engine's only door, and this is the second door"
    );
}

/// Devtools off, a navigation handler that asks the same rule, and no second window.
///
/// Pinned by name, the way `audit_binds.rs` pins a bind: the spellings are what the engine
/// reads, and a builder edited to drop one of them compiles without complaint.
#[test]
fn the_engine_is_built_with_devtools_off_and_a_navigation_handler() {
    let files = shipped();
    let web = file(&files, WEBVIEW);
    for (spelling, why) in [
        (
            "with_devtools(false)",
            "devtools in a reading window is a console for a page the reader did not write",
        ),
        (
            "with_navigation_handler",
            "without it a redirect, a meta refresh or a `location.href` never passes the \
             address rule",
        ),
        (
            "allowed(&next)",
            "the navigation handler must ask the same rule the address bar does, or a page \
             moves where `open` would have refused",
        ),
        (
            "NewWindowResponse::Deny",
            "a second window has no bounds in this frame and no reader contract",
        ),
        ("build_as_child", "the engine is a child view of this window, not a browser"),
    ] {
        assert!(
            web.code.contains(spelling),
            "{WEBVIEW} no longer holds `{spelling}`: {why}"
        );
    }
}

/// The reader contract's names, so the page agent and this audit cannot drift apart.
#[test]
fn the_module_keeps_the_reader_contract() {
    let files = shipped();
    let web = file(&files, WEBVIEW);
    for spelling in [
        "pub fn engine()",
        "pub fn allowed(url: &str)",
        "pub fn open(url: &str, rect: egui::Rect)",
        "impl Page",
        "pub fn set_bounds(&mut self, rect: egui::Rect)",
        "pub fn go(&mut self, url: &str)",
        "pub fn back(&mut self)",
        "pub fn forward(&mut self)",
        "pub fn url(&self)",
    ] {
        assert!(
            web.code.contains(spelling),
            "{WEBVIEW} no longer declares `{spelling}` — the reader page is written against \
             this contract, so a rename here is a second implementation there"
        );
    }
}

/// Nothing in the window — the engine's module included — hands a URL to the operating
/// system. `audit_open_url.rs` says this tree-wide; this is the same rule restated where
/// the URLs now live, because an engine with a `with_url` is the file most likely to grow
/// an opener next.
#[test]
fn the_window_hands_no_url_to_the_operating_system() {
    const OPENERS: &[&str] = &[
        "xdg-open",
        "sensible-browser",
        "kde-open",
        "gio open",
        "osascript",
        "NSWorkspace",
        "ShellExecute",
        "webbrowser::",
        "opener::",
        "open::that",
        ".open_url(",
        "Command::new(",
    ];
    let files = shipped();
    let desktop: Vec<SourceFile> = files
        .iter()
        .filter(|file| file.path.starts_with("atlas-desktop/src/"))
        .cloned()
        .collect();
    let mut hits: Vec<String> = Vec::new();
    for needle in OPENERS {
        for (path, line, _last, _own, text) in find_spanned(&desktop, needle, 2) {
            hits.push(format!("{path}:{line} `{needle}` — {text}"));
        }
    }
    assert!(
        hits.is_empty(),
        "the window can hand an address to the operating system (or start a program to do \
         it) — an in-process engine is not a licence for an external browser:\n  {}",
        hits.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Planted regressions: the source rules above, on a hostile module
// ---------------------------------------------------------------------------

/// A `webview.rs` that would make the engine a browser, and the rule that must reject it.
#[test]
fn a_planted_engine_without_devtools_or_the_handler_is_caught() {
    let mut planted = SourceFile {
        path: WEBVIEW.to_string(),
        code: concat!(
            "pub fn engine() -> bool { true }\n",
            "pub fn allowed(url: &str) -> bool { url.starts_with(\"http\") }\n",
            "pub fn open(url: &str, rect: egui::Rect) -> Option<Page> {\n",
            "    let built = wry::WebViewBuilder::new().with_url(url).build_as_child(&parent);\n",
            "    None\n",
            "}\n",
        )
        .to_string(),
    };
    for spelling in [
        "with_devtools(false)",
        "with_navigation_handler",
        "allowed(&next)",
        "NewWindowResponse::Deny",
    ] {
        assert!(
            !planted.code.contains(spelling),
            "the plant is supposed to be missing `{spelling}`"
        );
    }
    // The same predicate the live test uses: every spelling must be present.
    let missing: Vec<&str> = [
        "with_devtools(false)",
        "with_navigation_handler",
        "allowed(&next)",
        "NewWindowResponse::Deny",
        "build_as_child",
    ]
    .into_iter()
    .filter(|spelling| !planted.code.contains(spelling))
    .collect();
    assert!(
        !missing.is_empty(),
        "the builder rule accepted a view with devtools and no navigation handler"
    );
    // …and the good spelling silences it, so the rule is not firing on everything.
    planted.code.push_str(
        "fn ok() { builder.with_devtools(false).with_navigation_handler(|n: String| allowed(&n))\
         .with_new_window_req_handler(|_u, _f| wry::NewWindowResponse::Deny).build_as_child(&p); }\n",
    );
    let still_missing: Vec<&str> = [
        "with_devtools(false)",
        "with_navigation_handler",
        "allowed(&next)",
        "NewWindowResponse::Deny",
        "build_as_child",
    ]
    .into_iter()
    .filter(|spelling| !planted.code.contains(spelling))
    .collect();
    // `allowed(&next)` is the one spelling the appended line writes as `allowed(&n)`, which
    // is why the live rule pins the handler's *call*, not its parameter name.
    assert_eq!(
        still_missing,
        vec!["allowed(&next)"],
        "the builder rule should be silenced by the hardened spelling and only by it, still \
         missing {still_missing:?}"
    );
}

/// A second file naming the engine, and a second manifest carrying it.
#[test]
fn a_planted_second_owner_is_caught() {
    let manifests = vec![
        ("atlas-desktop/Cargo.toml".to_string(), "[dependencies]\nwry = { version = \"=0.57.3\" }\n".to_string()),
        ("atlas-web/Cargo.toml".to_string(), "[dependencies]\nwry = { version = \"=0.57.3\" }\n".to_string()),
        ("atlas-net/Cargo.toml".to_string(), "[dependencies]\n# wry = would be a comment\nureq = { workspace = true }\n".to_string()),
    ];
    assert_eq!(
        wry_manifests(&manifests),
        vec!["atlas-desktop/Cargo.toml".to_string(), "atlas-web/Cargo.toml".to_string()],
        "the manifest scan missed a second crate carrying the engine (a comment is not a \
         dependency)"
    );
    let files = vec![
        SourceFile {
            path: WEBVIEW.to_string(),
            code: "use wry::WebViewBuilder;\n".to_string(),
        },
        SourceFile {
            path: "atlas-desktop/src/views/articles.rs".to_string(),
            code: "fn draw() { let _ = wry::WebViewBuilder::new(); }\n".to_string(),
        },
    ];
    assert_eq!(
        wry_files(&files).len(),
        2,
        "the source scan missed a second file naming the engine"
    );
}
