//! The Reader page's privacy: a credential and a machine path never reach the screen.
//!
//! The Reader is the one page that shows an *address*. Everything else in the window draws
//! words this crate wrote or the archive's own prose; this page draws a URL, which is the one
//! string in an archive that routinely carries a credential (`https://user:pass@…`, a
//! presigned `?X-Amz-Signature=…`, a `?token=…`), and a briefing line, which the model wrote
//! and which may quote a path it was shown.
//!
//! Four rules, and the one function each of them belongs to:
//!
//! | Rule | Decided in |
//! | --- | --- |
//! | a URL with `user:pass` in it paints the **host** and nothing else | `views/reader.rs` (`toolbar`, `address`), `link::host` |
//! | a query string is never painted — a token lives there | `link::label` / `link::host` (split at `?`) |
//! | a path in a briefing line reaches the screen in the form the crate's own rule gives it | `assets::label` |
//! | no label on this page is unbounded | `views/reader.rs` (`one_line`, `paragraphs`, `clip`) |
//!
//! The third rule is deliberately phrased as *the same rule*, not a second one: `assets::label`
//! is the crate's one path-to-text reducer (the status row, the archive's own error sentences,
//! the graph's and the settings' file names all go through it), so this file calls it and
//! asserts its answer on screen. A test that copied the rule would keep passing while the
//! window drifted away from it.
//!
//! What is measured here is the painted text, not the source: egui answers every frame with the
//! shapes it drew, so the test runs the real page in a headless context and reads the galley of
//! every `Shape::Text`. That is the strongest statement available without a window: whatever the
//! code intends, this is what a reader sees.
//!
//! A red test in this file is a leak that needs a source change; the file does not patch the
//! view (it is another agent's while the Reader is being moved to the web view), it reports
//! `file:line`. The three that were open are closed and their `#[ignore]`s removed — the
//! reader no longer paints a signed-in address, the briefing card reduces model text, and
//! every reader label is bounded; the convention
//! `tests/disclosure.rs` set for this crate: the assertion is unchanged and still runs —
//! `cargo test --offline -p atlas-desktop --test reader_privacy -- --ignored` — so closing a
//! leak is deleting one attribute, and the default run says the rest of the page is clean.
//!
//! No log line is checked here; the crate writes no log at all, and that is pinned by
//! `tests/disclosure.rs` (`the_window_writes_no_file_at_all`). What this file adds on the same
//! subject is the narrower rule the Reader can break on its own: a print macro in a page module
//! is a log line nobody chose.

use std::path::Path;

use egui::{Pos2, Rect};

use atlas_desktop::assets;
use atlas_desktop::data::Briefing;
use atlas_desktop::link;
use atlas_desktop::views::{brief, reader};
use atlas_desktop::views::reader::Story;

/// The most a reader label may be. A label is one line in a window that is at most a screen
/// wide, so past a few hundred characters no reader is reading it any more: a stored title or
/// paragraph that long is a hostile or a broken row, and the page is what has to bound it.
/// The number is the brief's; it is stated once so a failure quotes it.
const MAX_LABEL: usize = 200;

// ---------------------------------------------------------------------------------------
// the harness: run one page headless and read what it painted
// ---------------------------------------------------------------------------------------

/// Collect every string egui painted, one entry per text shape.
///
/// Two frames, because a `ScrollArea` measures its content after drawing it: the first frame
/// lays the page out and the second is the one a reader sees.
fn paint(mut draw: impl FnMut(&mut egui::Ui)) -> Vec<String> {
    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);
    let mut painted: Vec<String> = Vec::new();
    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(900.0, 620.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| draw(ui));
        });
        painted.clear();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut painted);
        }
        output.drop_without_applying_deltas();
    }
    painted
}

fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
    match shape {
        egui::epaint::Shape::Text(text) => out.push(text.galley.text().to_owned()),
        egui::epaint::Shape::Vec(items) => {
            for item in items {
                walk(item, out);
            }
        }
        _ => {}
    }
}

/// One story, with the fields the page draws.
fn a_story(url: &str, title: &str, text: &str) -> Story {
    Story {
        id: 1,
        title: title.to_string(),
        source: "Example News".to_string(),
        published: "2026-11-22T10:00:00Z".to_string(),
        url: url.to_string(),
        text: text.to_string(),
        full_text: true,
    }
}

/// The Reader page in its text mode (no engine), which is the mode that draws every label.
fn paint_story(story: &Story) -> Vec<String> {
    paint(|ui| {
        reader::show(ui, story, None, false);
    })
}

/// The page as one string, so an assertion can say "appeared anywhere on the page".
fn page(story: &Story) -> String {
    paint_story(story).join("\n")
}

// ---------------------------------------------------------------------------------------
// 1 — a URL that carries a credential paints the host
// ---------------------------------------------------------------------------------------

/// `https://user:pass@host/story` must read `host` in the toolbar and must not spell the
/// credential anywhere on the page.
///
/// The two places the address reaches a label are the toolbar (`views/reader.rs`'s own
/// `host`) and the address line's copy button (`link::label`). Both ask
/// [`atlas_desktop::link::openable`] first, which refuses a host with `@` in it — the question
/// this test asks is what the page does **after** the refusal, because a refusal that prints
/// the address "so the reader knows" prints the password.
#[test]
fn a_credentialed_url_paints_the_host_and_not_the_credential() {
    let url = "https://reader:hunter2@news.example.com/story-1";

    // The gate's answer, measured: a credentialed address is not openable, so the page cannot
    // take the normal path and its fallback is what a reader sees.
    assert_eq!(
        link::openable(url),
        None,
        "the gate must refuse an address with a password in it"
    );

    let painted = page(&a_story(url, "A story", "Body text."));
    assert!(
        !painted.contains("hunter2"),
        "the password is painted on the Reader page; painted text was:\n{painted}"
    );
    assert!(
        !painted.contains("reader:"),
        "the user name is painted on the Reader page; painted text was:\n{painted}"
    );
    assert!(
        !painted.contains("user:pass"),
        "a credential form is painted on the Reader page; painted text was:\n{painted}"
    );
    assert!(
        painted.contains("news.example.com"),
        "the host must still be named — a label that hides its destination lies; \
         painted text was:\n{painted}"
    );

    // The label rule on the same address: `link::host` is what the copy button's text is
    // built from, so it must answer with the host and no user info.
    assert_eq!(
        link::host(url),
        "news.example.com",
        "link::host keeps the user info, so `Copy …` would carry the password"
    );
}

// ---------------------------------------------------------------------------------------
// 2 — a query string is never painted
// ---------------------------------------------------------------------------------------

/// A query is where a token lives (`?token=…`, `?X-Amz-Signature=…`), and no label needs it:
/// the host is what a reader is being told. This holds for both halves of the page — the
/// toolbar's host and the address line's copy button — and for a credentialed *and* a clean
/// address, because the two take different branches.
#[test]
fn a_query_holding_a_token_is_not_painted() {
    for url in [
        "https://news.example.com/story-1?token=T0PSECRET9&page=2",
        "https://news.example.com/story-1?X-Amz-Signature=deadbeefcafe&X-Amz-Credential=AKIA",
    ] {
        let painted = page(&a_story(url, "A story", "Body text."));
        for secret in ["T0PSECRET9", "deadbeefcafe", "X-Amz-Credential", "token=", "?page=2"] {
            assert!(
                !painted.contains(secret),
                "{secret:?} from the query string is painted on the Reader page for {url}; \
                 painted text was:\n{painted}"
            );
        }
        assert!(
            painted.contains("news.example.com"),
            "the host must be named for {url}; painted text was:\n{painted}"
        );
        // And the rule itself, so the reason is on the record: the label is built from the
        // host, and the host stops at the query.
        assert_eq!(link::label(url), "Copy news.example.com");
    }
}

// ---------------------------------------------------------------------------------------
// 3 — a path in a briefing line is reduced by the crate's own rule
// ---------------------------------------------------------------------------------------

/// A briefing is the model's sentence over the archive's rows, and a model that was shown a
/// path can repeat it. The crate already decides what a path may look like on screen —
/// [`atlas_desktop::assets::label`] — and this test asks that decision rather than inventing
/// one: it never spells the expected text itself.
#[test]
fn a_path_in_a_briefing_line_is_reduced_by_the_crates_own_label_rule() {
    let posix = "/Users/example/secret-notes.txt";
    let windows = r"C:\Users\example\secret-notes.txt";

    // The rule, called — not copied. It answers with the name a reader can act on.
    let reduced = assets::label(Path::new(posix));
    assert_eq!(
        reduced, "secret-notes.txt",
        "assets::label is the rule this test borrows; it changed shape"
    );

    let briefing = Briefing {
        scope: "window".to_string(),
        key: String::new(),
        hours: 72,
        watermark: 10,
        created: "2026-11-22T09:00:00Z".to_string(),
        line: format!("The archive was read from {posix} before the window opened."),
        bullets: vec![format!(
            "The model was handed {windows} and quoted it back"
        )],
        articles: 12,
        model: "test-model".to_string(),
        ms: 0,
    };
    let painted = paint(|ui| {
        brief::card(ui, Some(&briefing), "the window", false);
    });
    let joined = painted.join("\n");

    assert!(
        !joined.contains("/Users/"),
        "a home directory is painted in the briefing card; painted text was:\n{joined}"
    );
    assert!(
        !joined.contains(r"C:\Users") && !joined.contains(r"\Users\"),
        "a Windows home directory is painted in the briefing card; painted text was:\n{joined}"
    );
    assert!(
        joined.contains(&reduced),
        "the path was shown, and it must be shown the way assets::label says ({reduced:?}); \
         painted text was:\n{joined}"
    );
}

// ---------------------------------------------------------------------------------------
// 4 — no label is unbounded
// ---------------------------------------------------------------------------------------

/// Every string the page paints is a label in a window at most one screen wide, so none of
/// them may be unbounded: an archive row is a hostile row until the page has bounded it, and a
/// title or a paragraph of ten thousand characters is a page that cannot be laid out.
#[test]
fn no_reader_label_exceeds_two_hundred_characters() {
    let long_title = "T".repeat(400);
    let long_paragraph = "word ".repeat(200); // one block, no blank line: one paragraph
    let long_source = "S".repeat(300);
    let hostile = Story {
        source: long_source.clone(),
        ..a_story(
            "https://news.example.com/story-1",
            &long_title,
            &long_paragraph,
        )
    };
    let painted = paint_story(&hostile);
    assert!(!painted.is_empty(), "the page painted nothing at all");

    let over: Vec<(usize, String)> = painted
        .iter()
        .map(|label| (label.chars().count(), label.clone()))
        .filter(|(count, _)| *count > MAX_LABEL)
        .collect();
    assert!(
        over.is_empty(),
        "{} label(s) on the Reader page are longer than {MAX_LABEL} characters: {:?}",
        over.len(),
        over.iter()
            .map(|(count, label)| format!("{count} chars: {:?}", &label[..60.min(label.len())]))
            .collect::<Vec<_>>()
    );

    // The address line has its own bound, and it must hold for a long address too: a URL is
    // stored text and can be as long as its publisher wants.
    let address = format!("https://news.example.com/{}", "a".repeat(400));
    let painted = paint_story(&a_story(&address, "A story", "Body text."));
    for label in &painted {
        assert!(
            label.chars().count() <= MAX_LABEL,
            "a long address produced a {} character label: {:?}",
            label.chars().count(),
            &label[..60.min(label.len())]
        );
    }
}

// ---------------------------------------------------------------------------------------
// 5 — a page module does not print
// ---------------------------------------------------------------------------------------

/// The crate writes no log file (`tests/disclosure.rs`), and a page module printing to stdout
/// or stderr would be the same leak through the one door that is still open: a terminal, a
/// service log, a capture file. The rule is on the source because a print has no value to call.
#[test]
fn the_reader_modules_print_nothing() {
    let mut offenders = Vec::new();
    for (name, text) in reader_sources() {
        for (number, line) in shipped_lines(&text) {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if ["println!", "eprintln!", "print!", "eprint!", "dbg!"]
                .iter()
                .any(|macro_name| line.contains(macro_name))
            {
                offenders.push(format!("{name}:{number}: {}", trimmed));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the Reader page prints: {offenders:?}"
    );

    // The scan is not blind: it must see the module it is scanning, and it must refuse a
    // print when one is added to the text it reads.
    let (_, reader_rs) = reader_sources()
        .into_iter()
        .find(|(name, _)| name.ends_with("views/reader.rs"))
        .expect("views/reader.rs is the module this rule is about");
    assert!(
        reader_rs.contains("fn text_page"),
        "the scanner did not read the Reader's source"
    );
    assert!(
        shipped_lines("fn f() { eprintln!(\"leak\"); }")
            .iter()
            .any(|(_, line)| line.contains("eprintln!")),
        "the scan would not notice a print in shipped code"
    );
}

/// The reader's own modules: the page, and the card whose text it is next to. Read as text,
/// because only the source says which string is an argument of a print.
fn reader_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("views");
    ["reader.rs", "brief.rs"]
        .iter()
        .map(|name| {
            let path = root.join(name);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            (format!("src/views/{name}"), text)
        })
        .collect()
}

/// The lines of a source file that are *shipped*: everything before the first `#[cfg(test)]`.
///
/// The convention in this crate is one test module at the end of the file, and a print inside
/// it is a test's own business. A `#[cfg(test)]` on a single item above the module would make
/// the rest of the file look shipped, which is the direction that over-reports rather than
/// hides.
fn shipped_lines(source: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        lines.push((index + 1, line.to_string()));
    }
    lines
}
