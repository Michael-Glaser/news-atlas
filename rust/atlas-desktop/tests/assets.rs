//! Where the window's files come from, and what happens when one is not there.
//!
//! Four decisions live in `src/assets.rs` and one in `src/views/map.rs`, and none of
//! them had a test:
//!
//! * the project root is the **nearest** ancestor of the executable holding
//!   `rust/Cargo.toml`, and the working directory only when there is no such
//!   ancestor at all;
//! * the archive is `<root>/data/atlas.db` and nothing else;
//! * the outlines are the first **existing** of the three vendored names, in that
//!   order, and the first name is the answer when none of them exists;
//! * a missing (or unreadable, or non-GeoJSON) file is an **empty world**, never a
//!   panic — the window still opens and says so in the status row.
//!
//! # Why this file re-executes a copy of itself
//!
//! `assets::root()` reads `std::env::current_exe()`; it takes no argument, so there
//! is no way to ask it about a temporary tree from *this* process (the test binary
//! always lives under the checkout). The alternative was to test a copy of the
//! walk-up written here, which proves nothing about the code that ships. So the
//! test copies **this same test binary** into a temporary tree (`<tmp>/rust/target/
//! debug/deps/…`), runs it there with `--exact walker`, and reads back what the real
//! `root()`, `archive()` and `geojson()` answered from that position. The child runs
//! one test, prints three paths, opens no window and no socket, and the tree is
//! removed when the test ends. The same seam is missing for the vendor order, so the
//! order cases below are the same trick with a different `public/vendor/`.
//!
//! The fix that would make this file shorter is a parameter:
//! `assets::root_from(exe: &Path)` + `assets::geojson_in(root: &Path)` (the shape
//! this workspace already uses in `atlas_platform::root::discover(starts,
//! looks_like, max_hops)`). With those two, every case here is a temporary directory
//! and no child process — but neither exists, and a test may not invent the seam it
//! is supposed to be testing.
//!
//! # What this file does not decide
//!
//! Not the SQL: `tests/readonly_archive.rs` and `tests/sql_safety.rs` own the
//! queries and the read-only flag. Not the plate geometry: `tests/camera.rs` owns
//! frames, zoom and clustering. Here the map is only asked to survive an empty
//! world.

// The window, from the library: `atlas_desktop::assets` is the discovery this file
// is about, `data` is what the discovered path is opened with, `views::map` is the
// outline loader. Nothing is included by path — a copy of a module is a second
// implementation, and this file has to test the code that ships.
use atlas_desktop::{assets, data, views};

use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------- temp

/// A temporary directory, removed when the value is dropped.
///
/// The name is unique per process *and* per test, so two tests running in parallel
/// (and two `cargo test` runs) cannot share a tree.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Temp {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "atlas-desktop-assets-{}-{}-{}",
            name,
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temporary directory");
        Temp(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ------------------------------------------------------------------------- walker

/// Set in the child only. Without it the test below does nothing, so a normal
/// `cargo test` run of this target never re-enters.
const WALKER: &str = "ATLAS_DESKTOP_ASSETS_WALKER";

/// What the child reported: the three paths the window would use, plus whether the
/// archive it found opened and whether that open created a file.
struct Facts {
    root: PathBuf,
    archive: PathBuf,
    geojson: PathBuf,
    /// The name the window prints for that archive (`assets::label`, root-relative).
    archive_label: String,
    /// `ok`, or the sentence `Archive::open` answered.
    archive_open: String,
    /// Did the discovered archive exist after the window asked for it?
    archive_created: bool,
}

/// The child. It prints; the parent asserts, so every failure is reported once, by
/// name, with the tree that produced it.
#[test]
fn walker() {
    if std::env::var(WALKER).is_err() {
        return;
    }
    let archive = assets::archive();
    let opened = data::Archive::open(&archive);
    println!("root\t{}", assets::root().display());
    println!("archive\t{}", archive.display());
    println!("geojson\t{}", assets::geojson().display());
    // The name the status row and the sentences use: computed *here*, where `root()`
    // is this temporary tree, so the parent can compare against the same answer the
    // window would have computed rather than against its own root.
    println!("archive_label\t{}", assets::label(&archive));
    println!(
        "archive_open\t{}",
        match &opened {
            Ok(_) => "ok".to_string(),
            Err(sentence) => sentence.clone(),
        }
    );
    println!("archive_created\t{}", archive.exists());
}

/// Run a copy of this test binary from `root` and read the answers back.
///
/// `cwd` is what the child's working directory is set to: `root` for the cases where
/// the marker decides, and a separate directory for the one case where the working
/// directory is the fallback.
fn ask(root: &Path, cwd: Option<&Path>) -> Facts {
    let binary = root
        .join("rust")
        .join("target")
        .join("debug")
        .join("deps")
        .join(format!("atlas-desktop-walker{}", std::env::consts::EXE_SUFFIX));
    std::fs::create_dir_all(binary.parent().expect("the deps directory"))
        .expect("a deps directory in the temporary tree");
    if !binary.is_file() {
        // Once per tree: the child is only ever *this* binary, and a test that asks
        // five questions of one tree should not copy it five times.
        let this_test_binary = std::env::current_exe().expect("this test binary has a path");
        std::fs::copy(&this_test_binary, &binary).expect("a copy of this test binary");
    }

    let output = std::process::Command::new(&binary)
        .env(WALKER, "1")
        // One test, uncaptured: the child must not run the rest of the suite (it is
        // the whole window in a binary) and must not have its answer swallowed.
        .args(["--exact", "walker", "--nocapture", "--test-threads=1"])
        .current_dir(cwd.unwrap_or(root))
        .output()
        .expect("the copied test binary runs");
    assert!(
        output.status.success(),
        "the walker at {} exited {:?}\nstdout:\n{}\nstderr:\n{}",
        binary.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let text = String::from_utf8_lossy(&output.stdout);
    // The child is a libtest binary too, so its harness writes `test walker ... `
    // on the same line as the first answer. Every field is therefore looked for as
    // `key<TAB>value` anywhere in a line, not as a whole line.
    let field = |key: &str| -> String {
        let needle = format!("{key}\t");
        text.lines()
            .find_map(|line| {
                line.find(&needle)
                    .map(|at| line[at + needle.len()..].trim_end().to_string())
            })
            .unwrap_or_else(|| panic!("the walker did not report {key:?}; it said:\n{text}"))
    };
    Facts {
        root: PathBuf::from(field("root")),
        archive: PathBuf::from(field("archive")),
        geojson: PathBuf::from(field("geojson")),
        archive_label: field("archive_label"),
        archive_open: field("archive_open"),
        archive_created: field("archive_created") == "true",
    }
}

/// Two paths, compared as the file system resolves them.
///
/// `/var` is a symlink to `/private/var` on macOS, so the child's answer and this
/// test's `temp_dir()` answer can be two spellings of one directory. Every
/// comparison below goes through this.
fn same(left: &Path, right: &Path) -> bool {
    // The archive is a file that does not exist yet, so `canonicalize` cannot answer
    // for it: the deepest ancestor that *does* exist is resolved and the rest is
    // appended — the same rule `atlas_platform::root::canonical_text` uses for a
    // model file that has not been downloaded. This crate does not depend on that
    // one, and a test is the wrong place to add a dependency.
    fn resolve(path: &Path) -> PathBuf {
        let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
        let mut here = path;
        loop {
            if let Ok(real) = std::fs::canonicalize(here) {
                let mut out = real;
                for part in rest.iter().rev() {
                    out.push(part);
                }
                return out;
            }
            match (here.file_name(), here.parent()) {
                (Some(name), Some(parent)) => {
                    rest.push(name);
                    here = parent;
                }
                _ => return path.to_path_buf(),
            }
        }
    }
    resolve(left) == resolve(right)
}

/// Write the marker the walk-up looks for.
fn marker(root: &Path) {
    let manifest = root.join("rust").join("Cargo.toml");
    std::fs::create_dir_all(manifest.parent().expect("rust/")).expect("rust/");
    std::fs::write(&manifest, "[workspace]\nmembers = []\n").expect("the marker");
}

// ------------------------------------------------------- the walk up, in a temp tree

#[test]
fn the_walk_up_finds_the_marker_and_puts_the_archive_beside_it() {
    let tree = Temp::new("walk");
    marker(tree.path());
    // The child's working directory is deliberately *not* the root: "found by
    // walking up from the executable" and "the working directory" are the two
    // answers `root()` can give, and a test that sets them to the same directory
    // cannot tell them apart.
    let elsewhere = tree.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a working directory that is not the root");

    let facts = ask(tree.path(), Some(&elsewhere));
    assert!(
        same(&facts.root, tree.path()),
        "the root should be the ancestor holding rust/Cargo.toml ({}), not {}",
        tree.path().display(),
        facts.root.display()
    );
    // `<root>/data/atlas.db`, spelled out: the archive is not looked for anywhere
    // else, and it is not next to the binary. The first assertion is path text only
    // (no file system involved); the second is the same claim with both spellings of
    // the temporary directory resolved.
    assert_eq!(
        facts.archive,
        facts.root.join("data").join("atlas.db"),
        "the archive must be exactly <root>/data/atlas.db, and answered {}",
        facts.archive.display()
    );
    assert!(
        same(&facts.archive, &tree.join("data/atlas.db")),
        "the archive should be <root>/data/atlas.db ({}), not {}",
        tree.join("data/atlas.db").display(),
        facts.archive.display()
    );
    assert_eq!(
        facts.archive.file_name().and_then(|name| name.to_str()),
        Some("atlas.db")
    );
    assert_eq!(
        facts
            .archive
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str()),
        Some("data")
    );
}

#[test]
fn the_nearest_marker_wins_over_one_further_up() {
    // The window is started from `rust/target/release`, but a nested checkout would
    // put a second marker nearer to the binary. "Nearest" is the rule; a walk that
    // kept climbing would answer a different archive depending on where the tree sat.
    let tree = Temp::new("nearest");
    marker(tree.path());
    let nearer = tree.join("rust/target/debug");
    marker(&nearer);
    let elsewhere = tree.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a working directory that is not the root");

    let facts = ask(tree.path(), Some(&elsewhere));
    assert!(
        same(&facts.root, &nearer),
        "the root should be the nearest ancestor with the marker ({}), not {}",
        nearer.display(),
        facts.root.display()
    );
    assert!(
        same(&facts.archive, &nearer.join("data/atlas.db")),
        "the archive follows the nearest root, and answered {}",
        facts.archive.display()
    );
}

#[test]
fn with_no_marker_anywhere_the_working_directory_is_the_root() {
    // The fallback: an installed copy in `~/Applications` must still open *some*
    // window, and it says what it is missing instead of refusing to start.
    let tree = Temp::new("fallback");
    let working = tree.join("somewhere/else");
    std::fs::create_dir_all(&working).expect("a working directory");

    // Precondition: nothing above the temporary tree may hold the marker, or the
    // walk-up (correctly) wins and this case cannot be observed. `TMPDIR` under a
    // checkout would do that; say so instead of asserting on the machine.
    let mut above: Option<&Path> = Some(tree.path());
    while let Some(candidate) = above {
        if candidate.join("rust").join("Cargo.toml").is_file() {
            println!(
                "skipped: {} holds rust/Cargo.toml, so the fallback is unreachable here",
                candidate.display()
            );
            return;
        }
        above = candidate.parent();
    }

    let facts = ask(tree.path(), Some(&working));
    assert!(
        same(&facts.root, &working),
        "with no marker the working directory is the root ({}), not {}",
        working.display(),
        facts.root.display()
    );
    assert!(
        same(&facts.archive, &working.join("data/atlas.db")),
        "the archive follows the fallback root, and answered {}",
        facts.archive.display()
    );
}

#[test]
fn the_three_vendored_names_are_tried_in_order_and_the_first_is_the_fallback() {
    // The order is the whole decision: `ne_50m_countries.geojson`, then
    // `ne_50m_countries.geo.json`, then `countries.geo.json`, and when none of the
    // three exists the answer is the first name — a path the caller then finds
    // missing, which is an empty world rather than a directory listing or a guess.
    let tree = Temp::new("order");
    marker(tree.path());
    let vendor = tree.join("public/vendor");
    std::fs::create_dir_all(&vendor).expect("public/vendor");

    let first = "ne_50m_countries.geojson";
    let second = "ne_50m_countries.geo.json";
    let third = "countries.geo.json";

    // (the files present, the answer expected)
    let cases: [(&[&str], &str); 5] = [
        (&[first, second, third], first), // all three: the first is the answer
        (&[first, third], first),         // the second is gone; the order still holds
        (&[second, third], second),       // only the later two: the second wins
        (&[third], third),                // only the last one exists
        (&[], first),                     // none: the fallback is the first name
    ];

    for (present, expected) in cases {
        // rebuild the vendor directory for this case
        for name in [first, second, third] {
            let _ = std::fs::remove_file(vendor.join(name));
        }
        for name in present {
            std::fs::write(vendor.join(name), "{}").expect("a vendored file");
        }

        let facts = ask(tree.path(), None);
        assert!(
            same(&facts.geojson, &vendor.join(expected)),
            "with {present:?} present the outlines should be {expected}, not {}",
            facts.geojson.display()
        );
        // What the window will actually read: only the present names are files.
        assert_eq!(
            facts.geojson.is_file(),
            present.contains(&expected),
            "{} should {} exist for case {present:?}",
            facts.geojson.display(),
            if present.contains(&expected) { "" } else { "not" }
        );
    }
}

// ------------------------------------------- a missing file is a sentence, not a panic

#[test]
fn a_missing_archive_is_a_sentence_and_is_not_created() {
    // The window is read-only by design, so the discovered path is only ever opened
    // for reading: a missing archive must be a sentence in the status row and must
    // stay missing. A window that created `data/atlas.db` on the way past would put
    // an empty archive next to an installed copy and hide the real one.
    let tree = Temp::new("noarchive");
    marker(tree.path());
    let archive = tree.join("data/atlas.db");
    assert!(!archive.exists(), "the fixture starts without an archive");

    let facts = ask(tree.path(), None);
    assert!(!facts.archive_created, "asking for the archive created it");
    assert!(
        !archive.exists(),
        "the discovered archive was created at {}",
        archive.display()
    );
    assert_ne!(
        facts.archive_open, "ok",
        "a missing archive opened successfully"
    );
    // The sentence names the file, so the reader can act on it — and it names it the
    // way the status row does, through `assets::label`, which is root-relative
    // (`data/atlas.db`) and never the absolute path: a home directory on that row
    // tells a passer-by who the reader is. `archive_label` is what the child computed
    // with *its* root, so this comparison is the window's answer against the window's
    // answer. The two assertions after it hold the privacy half.
    assert!(
        !facts.archive_label.is_empty(),
        "the window named the archive with an empty string"
    );
    assert!(
        facts.archive_open.contains(&facts.archive_label),
        "the sentence should name the archive as {:?}; it said {:?}",
        facts.archive_label,
        facts.archive_open
    );
    assert!(
        !facts.archive_open.contains(&facts.archive.display().to_string()),
        "the sentence printed the absolute path around the archive: {:?}",
        facts.archive_open
    );
    assert!(
        !facts.archive_open.contains(&facts.root.display().to_string()),
        "the sentence printed the root: {:?}",
        facts.archive_open
    );
    assert!(
        facts.archive_open.contains("atlas.db"),
        "the sentence should say which file is missing; it said {:?}",
        facts.archive_open
    );
}

// ------------------------------------------------------------- the empty world

#[test]
fn a_missing_geojson_is_an_empty_world_not_a_panic() {
    let tree = Temp::new("nooutlines");
    let missing = tree.join("public/vendor/ne_50m_countries.geojson");

    // 1. the file is not there at all
    let world = views::map::World::load(&missing);
    assert_eq!(world.lines(), 0, "a missing file became coastlines");
    assert_eq!(world.points(), 0, "a missing file became points");
    assert_eq!(world.parse_ms, 0.0, "a file that was never read took time to read");

    // 2. a directory where the file should be: same answer, no panic
    let directory = tree.join("public/vendor/countries.geo.json");
    std::fs::create_dir_all(&directory).expect("a directory with the file's name");
    assert_eq!(views::map::World::load(&directory).lines(), 0);

    // 3. text that is not GeoJSON: the shipped file is somebody else's bytes, so
    //    every shape of "wrong" has to end in an empty world.
    for text in [
        "",
        "{}",
        "not json at all",
        "[]",
        r#"{"features":3}"#,
        r#"{"features":[]}"#,
        r#"{"features":[{}]}"#,
        r#"{"features":[{"geometry":null}]}"#,
        r#"{"features":[{"geometry":{"type":"Polygon","coordinates":[["x"]]}}]}"#,
    ] {
        let world = views::map::World::from_geojson(text);
        assert_eq!(world.lines(), 0, "{text:?} became coastlines");
        assert_eq!(world.points(), 0, "{text:?} became points");
    }

    // 4. the view that owns the world: it still opens, and says what it has.
    let view = views::map::MapView::new(&missing);
    assert!(
        view.outlines().starts_with("0 coastlines · 0 points"),
        "the status sentence should say the plate is empty; it said {:?}",
        view.outlines()
    );
    assert_eq!(view.segments(), 0);
}

#[test]
fn a_present_geojson_becomes_outlines_so_the_empty_world_is_not_the_only_answer() {
    // The control for the test above: the same call, with a file that is there.
    // Without this, "no coastlines" would be the answer for every input.
    let tree = Temp::new("outlines");
    let path = tree.join("public/vendor/ne_50m_countries.geojson");
    std::fs::create_dir_all(path.parent().expect("vendor/")).expect("vendor/");
    std::fs::write(
        &path,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Polygon","coordinates":[
                [[-10,-10],[10,-10],[10,10],[-10,10],[-10,-10]]]}},
            {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[
                [[[20,20],[30,20],[30,30],[20,30],[20,20]]],
                [[[40,40],[50,40],[50,50],[40,50],[40,40]]]]}}
        ]}"#,
    )
    .expect("a GeoJSON file");

    let world = views::map::World::load(&path);
    assert_eq!(world.lines(), 3, "two polygons and a multipolygon are three rings");
    assert_eq!(world.points(), 15, "five coordinates per closed ring");
    assert_eq!(views::map::MapView::new(&path).segments(), 0, "no frame has been drawn yet");
}

#[test]
fn an_empty_world_still_draws_its_plate_without_panicking() {
    // "An empty map instead of a panic" is only true if the frame draws: the
    // graticule and the pins are drawn from the camera, not from the outlines.
    let tree = Temp::new("frame");
    let missing = tree.join("public/vendor/ne_50m_countries.geojson");
    let mut view = views::map::MapView::new(&missing);

    let context = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 600.0),
        )),
        ..Default::default()
    };
    let mut clicked = None;
    let mut shapes = 0usize;
    // `egui 0.37`: the frame is `Context::run_ui`, which hands the callback the
    // *root* `Ui`; a panel is shown inside it, which is the shape `app::Atlas` uses.
    // Two frames, because the first lays the panel out and the second paints it in
    // the space it landed in — `tests/article_page.rs` draws its page the same way.
    for _ in 0..2 {
        let output = context.run_ui(input.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                clicked = view.show(ui, &[], 470.0);
            });
        });
        shapes = output.shapes.len();
        // A `FullOutput` carries texture deltas that only a renderer can upload.
        // A headless frame has no renderer, and `epaint` panics on a `FullOutput`
        // dropped with them still in it (the debug assertion this test used to
        // trip), so they are released here — the only thing a test with no GPU can
        // do with them. The shapes have already been read above.
        output.drop_without_applying_deltas();
    }

    assert!(clicked.is_none(), "an empty plate answered a click");
    assert_eq!(view.segments(), 0, "an empty world drew segments");
    // The graticule is drawn from the camera alone: an empty world is a plate with
    // lines on it, not a blank rectangle. Without this the test would pass on a
    // frame that drew nothing at all.
    assert!(
        shapes > 0,
        "an empty world drew no graticule: the plate painted no shapes"
    );
}

#[test]
fn the_outlines_this_tree_ships_are_a_file_the_window_can_read() {
    // The join between discovery and the map, on the real checkout, read-only: the
    // path `assets::geojson()` answers is one `World::load` can use. A tree without
    // the vendored file is not a failure here — tolerating that is the design pinned
    // above — but then this test says so instead of pretending it checked.
    let path = assets::geojson();
    // Said out loud, so a reader of the run sees what the code answers on this
    // checkout and not only that four assertions were satisfied.
    println!(
        "root\t{}\narchive\t{}\ngeojson\t{}",
        assets::root().display(),
        assets::archive().display(),
        path.display()
    );
    assert_eq!(
        path.parent().and_then(|dir| dir.file_name()).and_then(|name| name.to_str()),
        Some("vendor"),
        "the outlines should come from public/vendor, and answered {}",
        path.display()
    );
    if !path.is_file() {
        println!(
            "skipped: no vendored outlines at {} — the window opens with an empty plate",
            path.display()
        );
        return;
    }
    let world = views::map::World::load(&path);
    assert!(
        world.lines() > 0,
        "{} is a file but it parsed into no coastlines",
        path.display()
    );
}
