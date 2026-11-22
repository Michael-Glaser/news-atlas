//! What the window does on a machine that has never been set up.
//!
//! Three questions, each answered by something the window can be measured doing:
//!
//! * **Does it open, and does it say what is missing — by name?** [`crate::assets`-side
//!   decisions] are values here ([`assets::model`], [`assets::server`]), so a fresh tree
//!   is a fixture and a sentence; the window half is one headless frame of the real
//!   [`Atlas`] against an archive that is not there.
//! * **Is the model server's absence stated, with one retry and no spin?** The sentence a
//!   dead server produces comes from `http` and is measured for *time* here (a socket
//!   closed before the call is one refusal in milliseconds, not thirty seconds), and the
//!   frame's repaint request is measured for *rate*: an idle window asks for no frame at
//!   all, which is what makes a busy loop impossible rather than unlikely.
//! * **Is there no run-time download path?** Proven structurally, from the manifest the
//!   window ships with and from the source it compiles: four dependencies, none of which
//!   speaks HTTP; no call anywhere in `src/` that writes, renames, copies or listens; and
//!   the model manifest's `url` is never read, only its `name`.
//!
//! `tests/assets.rs` covers where the files are; this file covers what a reader is told
//! when they are not, and it lives apart because that file is being edited elsewhere.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atlas_desktop::app::{Atlas, Page, PAGES};
use atlas_desktop::{assets, http, theme};
use egui::epaint::{ClippedShape, Shape};
use egui::{Pos2, Rect, Vec2};

// ---------------------------------------------------------------------------
// a temporary tree, and the window that reads it
// ---------------------------------------------------------------------------

/// A directory under the system temporary directory, removed when the test ends.
///
/// It is `0700` and its name carries the process id and a counter, so two tests running at
/// once cannot share one — the same shape `tests/assets.rs` uses.
struct Tree {
    at: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let at = std::env::temp_dir().join(format!(
            "atlas-fresh-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            tag
        ));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).expect("a temporary directory");
        Tree { at }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.at.join(name);
        std::fs::create_dir_all(&path).expect("a directory in the tree");
        path
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.at.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory in the tree");
        }
        std::fs::write(&path, contents).expect("a file in the tree");
        path
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.at);
    }
}

const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// One frame of the shipped window, with the frame's texture deltas released.
///
/// `epaint` panics on `Drop` when a `TexturesDelta` still carries unapplied deltas; a real
/// backend uploads them, and nothing here reads a texture, so they are cleared — the
/// handling that crate asks for (`tests/smoke.rs` states the same reason at length).
fn frame(atlas: &mut Atlas, ctx: &egui::Context, index: usize) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(index as f64 * (1.0 / 60.0)),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| atlas.draw(ui));
    output.textures_delta.clear();
    output
}

/// Every string that reached a galley in one frame, joined by newlines.
fn text_of(output: &egui::FullOutput) -> String {
    fn walk(shape: &Shape, text: &mut String) {
        match shape {
            Shape::Vec(items) => {
                for item in items {
                    walk(item, text);
                }
            }
            Shape::Text(glyphs) => {
                text.push_str(glyphs.galley.text());
                text.push('\n');
            }
            _ => {}
        }
    }
    let mut text = String::new();
    for ClippedShape { shape, .. } in &output.shapes {
        walk(shape, &mut text);
    }
    text
}

/// How long the frame asked to wait before the next one — `Duration::MAX` when it asked
/// for nothing, which is the value that makes a busy loop impossible.
fn repaint_delay(output: &egui::FullOutput) -> Duration {
    output
        .viewport_output
        .values()
        .next()
        .expect("one viewport")
        .repaint_delay
}

// ---------------------------------------------------------------------------
// 1. a fresh machine: every piece named, and the window still opens
// ---------------------------------------------------------------------------

/// Nothing installed: no `models/`, no `bin/`, no manifest. Both answers must be a sentence
/// that names the file, the place and the command — the three things a reader can act on.
#[test]
fn a_machine_with_nothing_installed_is_told_what_to_install_by_name() {
    let tree = Tree::new("nothing");
    let model = assets::model_in(&tree.at.join("models"));

    assert!(!model.present());
    assert!(model.path().is_none(), "nothing may be named: {model:?}");
    let sentence = model.missing().expect("a sentence about the model");
    assert!(sentence.contains("models"), "names the directory: {sentence}");
    assert!(
        sentence.contains("atlas-setup model --yes"),
        "names the command: {sentence}"
    );
    assert!(
        !sentence.contains("://") && !sentence.contains("http"),
        "a row about a missing file carries no address: {sentence}"
    );

    let server = assets::server_in(&tree.at.join("bin"));
    assert!(!server.present());
    let sentence = server.missing().expect("a sentence about the server");
    assert!(sentence.contains("llama-server"), "names the binary: {sentence}");
    assert!(
        sentence.contains("atlas-setup llama --yes"),
        "names the command: {sentence}"
    );
}

/// The window itself, on a machine with no archive: it draws, and the status row says
/// which file is not there rather than "something went wrong".
#[test]
fn the_window_opens_with_no_archive_and_names_the_file_it_cannot_read() {
    let tree = Tree::new("no-archive");
    let missing = tree.at.join("data").join("atlas.db");
    assert!(!missing.exists());

    let ctx = context();
    let mut atlas = Atlas::with_archive(missing.clone());
    let painted = text_of(&frame(&mut atlas, &ctx, 0));

    // The archive's absence is a sentence, and it carries the label of the file — never
    // the temporary path the test happens to use.
    assert!(
        painted.contains("no archive at atlas.db"),
        "the status row did not name the archive:\n{painted}"
    );
    assert!(
        !painted.contains(&*tree.at.to_string_lossy()),
        "the row printed the path instead of the label:\n{painted}"
    );
    // …and it did not create it: a read-only window that creates its own archive would
    // answer the next reader with an empty one instead of this sentence.
    assert!(!missing.exists(), "the window created the archive it could not read");

    // Every page still draws. There is no data, so there is nothing on any of them — but
    // a page that panicked instead would be a window that does not open.
    for page in PAGES {
        atlas.set_page(page);
        let painted = text_of(&frame(&mut atlas, &ctx, 1));
        assert!(
            painted.contains("Refresh"),
            "the rail vanished on the {page:?} page:\n{painted}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. the model file: named by the manifest, and by the directory when it must be
// ---------------------------------------------------------------------------

/// The manifest `atlas-setup model` writes names the file the app runs. Absent, that file
/// must be reported **by name**, with the size it will have, and with the command.
#[test]
fn a_missing_model_file_is_named_with_its_size_and_the_command() {
    let tree = Tree::new("pinned");
    let models = tree.dir("models");
    // The size is the real pin's, so the sentence a reader sees on a fresh machine is the
    // sentence this test reads: 6.5 GB of weights, a name, and one command.
    tree.file(
        "models/model.json",
        br#"{"name":"pinned.gguf","url":"https://example.invalid/pinned.gguf","bytes":6526302432}"#,
    );

    let model = assets::model_in(&models);
    assert!(!model.present());
    let path = model.path().expect("the manifest named a file");
    assert_eq!(path, models.join("pinned.gguf"), "the name came from the manifest");

    let sentence = model.missing().expect("a sentence about the model");
    assert!(sentence.contains("pinned.gguf"), "names the file: {sentence}");
    // Named the way `label` names it: a fixture under /tmp reduces to the bare file name,
    // and the same value in a checkout reads `models/pinned.gguf` — the window never
    // prints the path it computed (tests/disclosure.rs, R4).
    assert!(
        sentence.contains(&assets::label(path)),
        "the sentence and the label disagree: {sentence}"
    );
    assert!(sentence.contains("6.5 GB"), "says how big it will be: {sentence}");
    assert!(
        sentence.contains("atlas-setup model --yes"),
        "names the command: {sentence}"
    );
    // Not the generic failure this test exists to rule out, and not the address either:
    // the window downloads nothing, so an `https://` in a row would be a promise.
    for generic in ["not found", "No such file", "://", "http", "error"] {
        assert!(
            !sentence.contains(generic),
            "{generic:?} in a sentence that must name a file and a command: {sentence}"
        );
    }
}

/// There and not there are the two answers, and an empty file is neither: a 0-byte
/// `pinned.gguf` is a transfer that stopped halfway, and calling it "there" is the lie the
/// next reader pays for.
#[test]
fn a_present_model_is_silent_and_an_empty_one_is_a_sentence() {
    let tree = Tree::new("present");
    let models = tree.dir("models");
    tree.file(
        "models/model.json",
        br#"{"name":"pinned.gguf","bytes":7}"#,
    );
    tree.file("models/pinned.gguf", b"weights");

    let model = assets::model_in(&models);
    assert!(model.present(), "{model:?}");
    assert_eq!(model.missing(), None, "a file that is there is not a row");

    tree.file("models/pinned.gguf", b"");
    let model = assets::model_in(&models);
    assert!(!model.present(), "an empty file is not a model: {model:?}");
    let sentence = model.missing().expect("a sentence about the empty file");
    assert!(
        sentence.contains("is empty") && sentence.contains("atlas-setup model --yes"),
        "{sentence}"
    );
}

/// With no manifest, the directory decides — and only by extension, so the half-written
/// `.part` of a transfer in flight is never reported as the model, and a name that could
/// leave `models/` is refused as a name.
#[test]
fn the_directory_decides_by_extension_and_refuses_a_name_that_could_leave_it() {
    let tree = Tree::new("directory");
    let models = tree.dir("models");

    // Only a partial transfer: nothing is named.
    tree.file("models/partial.gguf.part", b"half");
    assert!(
        !assets::model_in(&models).present(),
        "a .part file is not a model"
    );
    assert!(assets::model_in(&models).path().is_none());

    // Two models and no manifest: the newest name wins, the rule the launcher uses.
    tree.file("models/a.gguf", b"a");
    tree.file("models/b.gguf", b"b");
    assert_eq!(
        assets::model_in(&models).path(),
        Some(models.join("b.gguf").as_path())
    );

    // A manifest whose name points outside `models/` is refused as a name, so the
    // directory answers instead — and the answer stays inside `models/`.
    tree.file(
        "models/model.json",
        br#"{"name":"../../etc/passwd","bytes":11}"#,
    );
    let model = assets::model_in(&models);
    let path = model.path().expect("the directory still names a file");
    assert!(
        path.starts_with(&models),
        "a pinned name escaped models/: {path:?}"
    );
    assert_eq!(path, models.join("b.gguf"));
}

// ---------------------------------------------------------------------------
// 3. the model server: one sentence, bounded, and no spin
// ---------------------------------------------------------------------------

/// A server that is not there says so — with the address it tried and the command that
/// starts it — and it says so now: the call is a refusal in milliseconds, and the elapsed
/// time is measured here so "bounded" is a number and not a hope.
#[test]
fn a_dead_model_server_is_one_sentence_naming_the_address_and_the_command() {
    // A port with nothing behind it: bound here to learn a free one, then released, which
    // is exactly the state a machine with no server left running is in.
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port");
        listener.local_addr().expect("an address").port()
    };
    let url = format!("http://127.0.0.1:{port}");
    let at = format!("127.0.0.1:{port}");

    let started = Instant::now();
    let refused = http::ask_within(
        &url,
        "",
        &[],
        http::Budget::SHIPPED,
        Duration::from_secs(2),
    );
    let spent = started.elapsed();

    let sentence = refused.expect_err("no server, no answer");
    assert!(sentence.contains(&at), "names the address it tried: {sentence}");
    assert!(
        sentence.contains(http::START_COMMAND),
        "names the command that starts it: {sentence}"
    );
    // The bound that ran is milliseconds: `connect` to a closed loopback port is answered
    // by the kernel, so there is nothing here for a window to wait out, let alone loop on.
    assert!(
        spent < Duration::from_millis(500),
        "a closed port took {spent:?}, which is not a refusal but a wait"
    );
}

/// The other half of "no busy loop": what each page asks the backend to do while nothing
/// is happening. Every delay is measured, frame by frame, and the numbers are in the
/// failure message — a page that polls is a number here, not a claim in a comment.
///
/// `Duration::MAX` means "do not call me again until something happens". Anything below a
/// frame (16 ms) means the window asked to be called back as fast as the backend can,
/// which *is* the busy loop: it would run the CPU at 100 % while the model server is down
/// and nothing on screen changes.
///
/// The clock is monotonic across the whole run, because that is what a backend does. A
/// repeated `time` is not a slower window, it is an invalid input — measured, it makes
/// every later frame ask for an immediate repaint, which would be a spin this test
/// invented rather than one the app has.
///
/// Measured on a window with no archive (a machine that has never run Atlas), so what a
/// per-page figure means here is "the page asked this with nothing to animate":
///
/// | page | measured |
/// |------|----------|
/// | Overview, Topics, Monitors | `Duration::MAX` on every frame |
/// | Articles, Reader | `0ns` on the two frames after the switch, then `Duration::MAX` |
/// | Settings | `0ns` on the thirteen frames of egui's scroll-bar fade-in, then `Duration::MAX` |
/// | Graph | `16 ms` on every frame — see the note below |
/// | Chat | a countdown from wherever the caret's blink cycle is, one frame shorter each frame (`300 ms → 16.7 ms`, then it wraps to `500 ms`) — see the note below |
///
/// The Graph page's figure is a defect this test does not hide: with no archive there
/// are no nodes, `GraphView::tick` returns early when there is nothing to cool
/// (`src/views/graph.rs:384`), so `alpha` never falls and the shell keeps asking
/// (`src/app.rs:560`). Both files belong to other agents; the assertion below is the
/// invariant that must survive either fix — *the graph asks every 16 ms or not at all* —
/// and the report carries the rest.
#[test]
fn an_idle_window_never_asks_to_be_called_back_faster_than_a_frame() {
    let tree = Tree::new("repaint");
    let ctx = context();
    let mut atlas = Atlas::with_archive(tree.at.join("data").join("atlas.db"));
    // Two frames before anything is measured: a fresh `Context` builds its font atlas on
    // the frame it is created and asks for an immediate one to do it (measured `0ns`,
    // every page, animated or not). That is the context warming up, and it stops.
    let mut clock: usize = 0;
    for _ in 0..2 {
        let _ = frame(&mut atlas, &ctx, clock);
        clock += 1;
    }

    for page in PAGES {
        atlas.set_page(page);
        let mut delays = Vec::new();
        // Twenty frames, not twelve: egui fades a scroll bar in over `animation_time`
        // (0.25 s, 15 frames at this clock), and a window shorter than that cannot tell a
        // fade that ends from a loop that does not. Measured on Settings: thirteen
        // immediate frames and then `MAX` for the rest of the run.
        for _ in 0..20 {
            delays.push(repaint_delay(&frame(&mut atlas, &ctx, clock)));
            clock += 1;
        }

        // The invariant, on every page: no *loop*. A page switch costs two immediate
        // frames while the new text is rasterised (measured; the same two on Articles
        // below), and egui itself may ask for a third of a second of frames to fade
        // something in — both are bounded, both stop, and both are what a *first paint*
        // costs. What must never happen is an immediate frame once that first paint is
        // behind the page: sixteen frames is the fade (0.25 s / 16.67 ms = 15) plus one,
        // so a burst that reaches past it is a spin this page owns.
        const FADE_FRAMES: usize = 16;
        let immediate = delays
            .iter()
            .filter(|delay| **delay < Duration::from_millis(16))
            .count();
        assert!(
            immediate <= FADE_FRAMES,
            "the {page:?} page asked for {immediate} of twenty idle frames immediately: {delays:?}"
        );
        assert!(
            delays[FADE_FRAMES..]
                .iter()
                .all(|delay| *delay >= Duration::from_millis(16)),
            "the {page:?} page asked for an immediate frame after its first paint: {delays:?}"
        );

        match page {
            // Nothing on these pages animates, so once their first paint is behind them an
            // idle frame must ask for nothing.
            //
            // The two-frame allowance is the same one the Articles branch makes, for the same
            // measured reason: the first frame that paints a *new* string grows the font atlas,
            // and egui answers that by asking for one more frame. Monitors now paints text it
            // did not before (the "Add a monitor" field and its link), so it costs that once at
            // the start of the run and then goes quiet — measured: `[0ns, 0ns, MAX, …]` with the
            // field, `[MAX, …]` without. That is a layout, not a loop, and a *loop* is what this
            // page's assertion is for: every frame after the first two must be exactly `MAX`,
            // which is stricter than the `>= 16 ms` rate the pages below are held to.
            Page::Overview | Page::Topics | Page::Monitors => assert!(
                delays[2..].iter().all(|delay| *delay == Duration::MAX),
                "the {page:?} page asked for a frame with nothing to animate: {delays:?}"
            ),
            // The Settings page is a static form and asks for nothing of its own. It does
            // now paint a scroll area long enough to need a bar, and egui fades that bar
            // in with `animate_bool_responsive` (`egui/src/containers/scroll_area.rs:833`),
            // which asks an immediate frame for the length of `animation_time` — measured
            // on this fixture, twenty frames: `[0ns × 13, MAX, MAX, …]`. So the page is
            // held to the rule the whole run is held to (nothing immediate once the fade
            // is behind it) and to rest for good after it, which is what "static" means.
            Page::Settings => assert!(
                delays[16..].iter().all(|delay| *delay == Duration::MAX),
                "the Settings page asked for a frame with nothing to animate: {delays:?}"
            ),
            // A page switch is a new layout: measured two immediate frames, then rest.
            Page::Articles => assert!(
                delays[2..].iter().all(|delay| *delay == Duration::MAX),
                "the Articles page did not settle after its layout: {delays:?}"
            ),
            // The Reader is a static page too: with no story it is one sentence, and with
            // one it is either the archive's text or the engine, which paints itself and
            // asks egui for nothing. Its first layout is a new page like Articles', so the
            // same two-frame allowance and then rest.
            Page::Reader => assert!(
                delays[2..].iter().all(|delay| *delay == Duration::MAX),
                "the Reader page did not settle after its layout: {delays:?}"
            ),
            // The composer takes focus when the page opens (`chat.rs:1312`), so egui
            // paints a blinking caret and asks to be woken at the *edge* of the blink:
            // `paint_text_cursor` computes `wake_in` as the time left until the caret
            // flips (`egui-0.37.0/src/text_selection/visuals.rs:305`). That is a
            // countdown, not a rate. Measured on this fixture, twelve frames:
            // `[0ns, 0ns, 233.3ms, 216.7ms, 200.0ms, 183.3ms, 166.7ms, 150.0ms, 133.3ms,
            // 116.7ms, 100.0ms, 83.3ms]` — exactly one 16.67 ms frame less each time. The
            // start depends on where the blink cycle happens to be when the page is
            // switched to (433 ms in an earlier run of this same fixture), so a *floor*
            // would only pin the fixture's phase; it was asserted here until the page
            // grew a text the caret sits in. What is true of every phase is the bound and
            // the shape: the caret asks at most `on_duration` = 500 ms
            // (`egui/src/style.rs:1084`), the pending question asks 100 ms
            // (`chat.rs:982`), the setup poll 500 ms (`chat.rs:909`), so no ask may be
            // longer than half a second — and a countdown may never turn into a spinner:
            // three equal waits under 100 ms in a row would be one (the polls are
            // `>= 100 ms`, so an equal small repeat can only be a loop this page added).
            Page::Chat => {
                assert!(
                    delays[2..]
                        .iter()
                        .all(|delay| *delay == Duration::MAX
                            || *delay <= Duration::from_millis(500)),
                    "the Chat page asked to be called back later than its slowest poll: {delays:?}"
                );
                let repeated = delays[2..]
                    .windows(3)
                    .any(|w| w[0] == w[1] && w[1] == w[2] && w[0] < Duration::from_millis(100));
                assert!(
                    !repeated,
                    "the Chat page asked for one small wait three times running: {delays:?}"
                );
            }
            // 16 ms while the layout cools, nothing at all once it has. With data, that
            // is ~6 s of frames after a click; with no archive it never stops, which is
            // the defect the doc comment above names.
            //
            // The first two frames are exempt, exactly as they are on Articles and Reader
            // above and for the same measured reason: the Graph page now paints text it
            // did not before (the `Zoom in` / `Zoom out` / `Fit` row and the caption), so
            // the switch grows the font atlas and egui answers that by asking for one more
            // frame — measured on this fixture with no archive, twenty frames:
            // `[0ns, 0ns, 16ms, 16ms, … 16ms]`. What this branch pins is the *sustained*
            // rate, and an immediate ask anywhere after the layout still fails it.
            Page::Graph => assert!(
                delays[2..]
                    .iter()
                    .all(|delay| *delay == Duration::MAX || *delay == Duration::from_millis(16)),
                "the Graph page asked for something other than a 60 Hz cooling frame: {delays:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// 4. no download path exists
// ---------------------------------------------------------------------------

/// The manifest the window ships with is the whole of its capability list, and it is read
/// here rather than described: four dependencies, none of them an HTTP client, and
/// `atlas-setup` — the one crate in this workspace that fetches — not among them.
#[test]
fn the_window_links_no_http_client_and_no_downloader() {
    let manifest = include_str!("../Cargo.toml");
    let mut names: Vec<String> = Vec::new();
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            names.push(name.trim().to_string());
        }
    }
    assert_eq!(
        names,
        vec!["eframe", "egui", "rusqlite", "serde_json"],
        "the window's dependencies changed; a download path would start here"
    );
    for client in ["ureq", "reqwest", "isahc", "hyper", "curl", "atlas-setup", "atlas_setup"] {
        assert!(
            !manifest.contains(client),
            "{client} is in the window's manifest, so a fetch is reachable from the window"
        );
    }
}

/// And nothing in the source can write a download to disk or listen for one: the words
/// that would have to appear are absent from every file under `src/`.
///
/// Comments are scanned too — deliberately. A mention is not a call, but a crate that says
/// "we never fetch" should be able to name what it does not do without using the calls it
/// does not make, and a scan that skipped comments could not tell a description from a
/// call in a macro.
#[test]
fn nothing_in_the_window_writes_a_file_or_listens() {
    const FORBIDDEN: [&str; 10] = [
        "fs::write",
        "File::create",
        "OpenOptions",
        "fs::copy",
        "fs::rename",
        "TcpListener",
        "std::process::Command",
        "Command::new",
        "reqwest",
        "ureq",
    ];

    fn files(directory: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("the source directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }

    let mut sources = Vec::new();
    files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    assert!(sources.len() > 10, "the scan found {} files", sources.len());

    for source in &sources {
        let text = std::fs::read_to_string(source).expect("a source file");
        // Two rules meet here, and both are about the *shipped* window.
        //
        // 1. A `#[cfg(test)]` block is skipped. Tests in this crate make temporary files —
        //    it is how a reader of files gets tested at all — and a scan that could not tell a
        //    test's temp directory from the product's write surface would force an exception
        //    per test file, which is how a rule stops meaning anything. (`no_spawn_no_socket.rs`
        //    does the same thing properly, by module reachability, and proves it on a planted
        //    tree; this file's scan is the cheap version of the same idea.) Braces are counted
        //    on the line, so a block spanning one line and a block spanning a hundred are both
        //    skipped; a miss here fails *loudly* (a forbidden word is found), never silently.
        //
        // 2. `src/requests.rs` is the one shipped exception, and it is a product decision
        //    rather than a loosened test: the window cannot ingest (the archive is opened
        //    `SQLITE_OPEN_READ_ONLY` with `PRAGMA query_only`), so a reader's "go and look" is
        //    a *request* — one row in `data/requests.db` that the supervised agent claims. No
        //    *other* file may name any of these shapes, so the write surface cannot grow a
        //    second target without a second decision.
        let is_the_queue = source.ends_with("src/requests.rs") || source.ends_with("src\\requests.rs");
        let mut inside_test = false;
        let mut waiting_for_a_body = false;
        let mut depth: i32 = 0;
        for line in text.lines() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                waiting_for_a_body = true;
            }
            if waiting_for_a_body && line.contains('{') {
                inside_test = true;
                waiting_for_a_body = false;
                depth = 0;
            }
            if !inside_test && !is_the_queue {
                for forbidden in FORBIDDEN {
                    assert!(
                        !line.contains(forbidden),
                        "{} contains {forbidden:?}, so the window can write or listen",
                        source.display()
                    );
                }
            }
            if inside_test {
                depth += line.matches('{').count() as i32;
                depth -= line.matches('}').count() as i32;
                if depth <= 0 {
                    inside_test = false;
                }
            }
        }
        if is_the_queue {
            // And the exception is really the queue: one read-write open, one file named.
            for needed in ["SQLITE_OPEN_READ_WRITE", "requests.db"] {
                assert!(
                    text.contains(needed),
                    "src/requests.rs no longer contains {needed:?}, so the allowance above \
                     describes code that is not there"
                );
            }
        }
    }
}

/// The manifest's `url` is the one address in this repository's model plan, and the window
/// never reads it: the value that comes back carries a path and a name, and the sentence
/// carries neither scheme nor host.
#[test]
fn the_model_manifest_is_read_for_its_name_and_never_for_its_address() {
    let tree = Tree::new("address");
    let models = tree.dir("models");
    tree.file(
        "models/model.json",
        br#"{"name":"m.gguf","url":"https://example.invalid/m.gguf","bytes":3}"#,
    );
    tree.file("models/m.gguf", b"abc");

    let model = assets::model_in(&models);
    assert!(model.present());
    assert_eq!(model.missing(), None);
    assert_eq!(model.path(), Some(models.join("m.gguf").as_path()));
    // Nothing the value can say out loud is an address: the window has no client to use
    // one with, and the summary line a reader sees must not promise a fetch.
    let shown = format!("{model:?}");
    assert!(!shown.contains("://"), "the address reached the value: {shown}");
}


/// One line, and it names the piece that is actually blocking: the server first, because a
/// server with no model refuses to start, and the model only once the server is there.
/// A machine with both says nothing — "why can I not chat" then has an answer that is not
/// a missing file, and a row that kept naming one would be wrong.
#[test]
fn the_chat_line_names_the_first_missing_piece_and_silences_itself() {
    let tree = Tree::new("chat-line");
    let models = tree.dir("models");
    let bin = tree.dir("bin");

    let sentence = assets::chat_missing_in(&models, &bin).expect("nothing is installed");
    assert!(sentence.contains("llama-server"), "the server blocks first: {sentence}");

    tree.file("bin/llama-server", b"\x7fELF");
    let sentence = assets::chat_missing_in(&models, &bin).expect("the model is still missing");
    assert!(
        !sentence.contains("llama-server"),
        "the server is there now: {sentence}"
    );
    assert!(sentence.contains("atlas-setup model --yes"), "{sentence}");

    tree.file("models/model.json", br#"{"name":"m.gguf","bytes":3}"#);
    tree.file("models/m.gguf", b"abc");
    assert_eq!(assets::chat_missing_in(&models, &bin), None);
}
