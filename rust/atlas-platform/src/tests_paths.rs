//! The path and permission suite: containment, `..`, links, and the modes a
//! private file is created with — over a **real** file system.
//!
//! It lives next to `tests.rs` rather than inside it because the rules it pins are
//! one concern (`atlas-platform`'s paths and permissions) and the two suites
//! together passed the size a readable file may have (`docs/RUST-STYLE.md`). What
//! it adds to `tests` is only a `TempDir` it borrows: the cases below need real
//! directories, real links and real modes, so they cannot be decided by text.
//!
//! Every case here failed on the code as it was before the security pass, and the
//! comment above it says what the old answer was. The measured numbers are in the
//! module docs they belong to (`crate::root::canonical_text`,
//! `crate::winpath::inside`, `crate::private`).

use std::path::Path;

use crate::tests::TempDir;
use crate::{host, location, private, root};

// ---------------------------------------------------------------------------
// a `..` in a path that does not exist yet: applied, never dropped
// ---------------------------------------------------------------------------

/// A link at `at` pointing at `target`, on the platforms that have links.
#[cfg(unix)]
fn link_to(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).expect("symlink");
}

#[test]
fn dot_dot_in_a_path_that_does_not_exist_yet_is_applied_and_not_dropped() {
    // `canonical_text` promises an answer for a path that is not there yet. The
    // earlier version popped the missing components, canonicalised the rest and
    // pasted them back — which **deleted every `..`** in the missing part, so
    // `app/not-yet/../../outside/secret.txt` answered
    // `app/not-yet/outside/secret.txt` and `inside_root` said `true` about a path
    // whose meaning is outside the root. Measured before the fix:
    //   canonical_text -> .../app/not-yet/outside/secret.txt
    //   inside_root    -> true
    let temp = TempDir::new("missing-downdot");
    let app = temp.join("app");
    let outside = temp.join("outside");
    std::fs::create_dir_all(&app).expect("app");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("secret.txt"), b"x").expect("secret");

    let climbing = app
        .join("not-yet")
        .join("..")
        .join("..")
        .join("..")
        .join("outside")
        .join("secret.txt");
    let text = root::canonical_text(&climbing);
    assert!(
        text.ends_with("/outside/secret.txt"),
        "the two `..` must climb out of app, not be dropped: {}",
        text
    );
    assert!(
        !text.contains("not-yet"),
        "the missing component `..` cancelled must be gone: {}",
        text
    );
    assert!(
        text.contains("outside/secret.txt") && !text.contains("app/not-yet"),
        "{}",
        text
    );
    assert!(
        !root::inside_root(&app, &climbing),
        "{} escaped {}",
        climbing.display(),
        app.display()
    );
    // A `..` that stays inside is still inside, so the rule did not become a
    // refusal of every missing path.
    let inside = app.join("data").join("..").join("models").join("m.gguf");
    assert!(root::inside_root(&app, &inside), "{}", inside.display());
    // And a relative path that climbs past its own start keeps its `..`.
    assert_eq!(root::canonical_text(Path::new("../x")), "../x");
    assert_eq!(root::canonical_text(Path::new("a/../..")), "..");
}

#[test]
fn a_data_dir_that_climbs_reports_the_place_it_really_lands_in() {
    // `ATLAS_DATA_DIR` with a `..` tail is the case that mattered: the answer the
    // containment check used described a path the app never touches. Measured
    // before the fix — `canonical_text` said `app/data/inner/elsewhere` and
    // `inside_root(app, …)` said `true`, while `create_dir_all` on the same text
    // really created `<temp>/elsewhere`, outside the root.
    let temp = TempDir::new("data-climb");
    let app = temp.join("app");
    std::fs::create_dir_all(app.join("data")).expect("app");
    let override_dir = app
        .join("data")
        .join("inner")
        .join("..")
        .join("..")
        .join("..")
        .join("elsewhere");

    assert!(
        !root::inside_root(&app, &override_dir),
        "a data dir that climbs out of the project must not be called inside it"
    );
    let text = root::canonical_text(&override_dir);
    // The text has to name the place the kernel will use. `create_dir_all` is
    // what the app itself calls to make a data dir, so it is the referee.
    std::fs::create_dir_all(&override_dir).expect("the app creates its data dir");
    assert!(
        temp.join("elsewhere").is_dir(),
        "the kernel really put it at {}/elsewhere",
        temp.path.display()
    );
    assert!(
        !app.join("data").join("inner").join("elsewhere").is_dir(),
        "and not at the place the old answer described"
    );
    assert!(
        text.ends_with(&format!(
            "{}{}elsewhere",
            temp.path.canonicalize().expect("temp").display(),
            std::path::MAIN_SEPARATOR
        )),
        "canonical_text must name where it lands: {}",
        text
    );
}

#[test]
fn dot_dot_after_a_symlinked_directory_resolves_against_the_target() {
    // `..` after a canonicalised symlink is not "the directory above the link":
    // it is the directory above the link's **target**, which is what the kernel
    // uses. A missing component after the link used to have the `..` dropped
    // instead of applied (`outside/deep/absent/../../secret.txt` answered
    // `outside/deep/absent/secret.txt`).
    let temp = TempDir::new("link-dotdot");
    let app = temp.join("app");
    let outside = temp.join("outside");
    let deep = outside.join("deep");
    std::fs::create_dir_all(&app).expect("app");
    std::fs::create_dir_all(&deep).expect("deep");
    std::fs::write(outside.join("secret.txt"), b"x").expect("secret");

    #[cfg(unix)]
    {
        let link = app.join("link");
        link_to(&deep, &link);
        // The link's target's parent is `outside`: absent/../.. from there is
        // `outside`, and `outside/secret.txt` is outside the project.
        let climb = link
            .join("absent")
            .join("..")
            .join("..")
            .join("secret.txt");
        assert_eq!(
            root::canonical_text(&climb),
            root::canonical_text(&outside.join("secret.txt")),
            "`..` after the link must cancel the link's target directory"
        );
        assert!(!root::inside_root(&app, &climb));
        // A path that stays under the link's target stays outside the project,
        // even though its text begins with the project's own directory.
        assert!(!root::inside_root(&app, &link.join("absent").join("note.txt")));
    }
}

// ---------------------------------------------------------------------------
// a link is not a private file, and a private file is never created through one
// ---------------------------------------------------------------------------

#[test]
fn a_link_is_never_chmodded_and_never_reported_private() {
    // `chmod` follows a link, so hardening a linked path changed the file at the
    // other end and then reported "this path is owner-only". Measured before the
    // fix: `make_file_private(data/config.json -> elsewhere)` answered `Changed`
    // and the file outside the project went from `0644` to `0600`.
    let temp = TempDir::new("link-private");
    let data = temp.join("data");
    std::fs::create_dir_all(&data).expect("data");
    let outside = temp.join("elsewhere-config.json");
    std::fs::write(&outside, b"{}").expect("config");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if private::modes_apply() {
            std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }
        let link = data.join("config.json");
        link_to(&outside, &link);

        assert!(private::is_link(&link));
        assert_eq!(private::make_file_private(&link), private::Privacy::Linked);
        assert!(!private::Privacy::Linked.ok(), "a link is not private");
        // The file at the other end is untouched — the point of the whole rule.
        if private::modes_apply() {
            assert_eq!(
                private::mode_of(&outside),
                Some(0o644),
                "a link to another file must not change that file's mode"
            );
        }
        // And the verdict is about the link, not about its target.
        let facts = private::facts(&link);
        assert!(facts.linked && !facts.ok, "{:?}", facts);
        assert_eq!(facts.note, private::Privacy::Linked.note());
        // A directory reached only through a link is not hardened either: the
        // caller should refuse the path, not chmod someone else's directory.
        let real_dir = temp.join("real-data");
        std::fs::create_dir_all(&real_dir).expect("real dir");
        let dir_link = temp.join("data-link");
        link_to(&real_dir, &dir_link);
        assert_eq!(private::make_dir_private(&dir_link), private::Privacy::Linked);
    }
    // An ordinary file is still hardened, so the rule did not become a refusal.
    let plain = data.join("atlas.db");
    std::fs::write(&plain, b"x").expect("plain");
    assert!(private::make_file_private(&plain).ok());
    // A path that does not exist and is not a link is `Missing`, not `Linked`.
    assert_eq!(
        private::make_file_private(temp.join("never-written.log")),
        private::Privacy::Missing
    );
}

#[test]
fn a_created_file_is_private_from_the_first_byte_and_never_through_a_link() {
    let temp = TempDir::new("create-options");
    let data = temp.join("data");
    std::fs::create_dir_all(&data).expect("data");

    // The mode comes from the `open`, so it is `0600` and not the `0644` a plain
    // write gets under the usual umask.
    let fresh = data.join("atlas.db");
    private::create_options()
        .open(&fresh)
        .expect("a fresh file is created");
    if private::modes_apply() {
        assert_eq!(
            private::mode_of(&fresh),
            Some(private::FILE_MODE),
            "0600 from the open, not from a later chmod"
        );
    }

    #[cfg(unix)]
    {
        // A name already taken by a link is refused, not followed. Measured
        // before the fix: the open succeeded through the link and a write landed
        // in the linked file.
        let victim = temp.join("victim.txt");
        std::fs::write(&victim, b"original").expect("victim");
        let link = data.join("atlas-linked.db");
        link_to(&victim, &link);
        let opened = private::create_options().open(&link);
        assert!(
            opened.is_err(),
            "an open for a file ATLAS creates must not follow a link"
        );
        // The link target is untouched: nothing was created and nothing written.
        assert_eq!(std::fs::read(&victim).expect("read"), b"original");
        // The "create only if it is not there" shape refuses the link too, so a
        // caller cannot be redirected to an existing file it did not create.
        assert!(private::create_new_options().open(&link).is_err());
        assert!(private::create_new_options().open(&fresh).is_err(),
            "create_new is create-new: an existing file is AlreadyExists");
        let brand_new = data.join("brand-new.db");
        assert!(private::create_new_options().open(&brand_new).is_ok());
    }
}

#[test]
fn a_directory_created_by_atlas_is_owner_only_from_the_mkdir() {
    // `create_dir_all` answers `0755` under the usual umask and the `chmod`
    // afterwards leaves a window in which another account opens the directory it
    // is about to be locked out of. `umask` can only clear bits, so a `mkdir` at
    // 0700 can never be wider than 0700.
    let temp = TempDir::new("create-dir");
    let plain = temp.join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    if private::modes_apply() {
        assert_eq!(
            private::mode_of(&plain),
            Some(0o755),
            "this test is only meaningful where create_dir_all is world-readable"
        );
    }
    let private_dir = temp.join("scratch");
    private::create_dir(&private_dir).expect("a fresh name is created");
    if private::modes_apply() {
        assert_eq!(private::mode_of(&private_dir), Some(private::DIR_MODE));
        assert!(private::facts(&private_dir).ok);
    }
    // A name that is already there is not adopted: another account can pre-create
    // it in a shared directory and keep a handle on everything written inside.
    let taken = private::create_dir(&private_dir);
    assert_eq!(
        taken.as_ref().err().map(|error| error.kind()),
        Some(std::io::ErrorKind::AlreadyExists)
    );
    // A link is `AlreadyExists` too, which is the refusal that matters.
    #[cfg(unix)]
    {
        let link = temp.join("scratch-link");
        link_to(&private_dir, &link);
        assert!(private::create_dir(&link).is_err());
    }
}

#[test]
fn a_read_only_open_never_follows_a_link() {
    let temp = TempDir::new("open-nofollow");
    let real = temp.join("index.html");
    std::fs::write(&real, b"<!doctype html>").expect("write");
    assert!(private::open_readonly_nofollow(&real).is_some());
    assert!(private::open_readonly_static(&real).is_some());
    #[cfg(unix)]
    {
        let link = temp.join("leak.html");
        link_to(&real, &link);
        // The caller asked for *this* file. A link is not it.
        assert!(
            private::open_readonly_nofollow(&link).is_none(),
            "an O_NOFOLLOW open must refuse a symlink"
        );
        assert!(private::open_readonly_static(&link).is_none());
        // And the platform has to be able to promise it at all: a unix whose
        // O_NOFOLLOW this crate has no constant for must not silently follow.
        assert_eq!(private::no_follow_supported(), host::is_windows() || private::no_follow_flag() != 0);
    }
}

// ---------------------------------------------------------------------------
// reach beyond the owner, and where private data is allowed to live
// ---------------------------------------------------------------------------

#[test]
fn execute_bits_are_reach_so_a_traversable_directory_is_not_private() {
    // An execute bit on a directory is traverse: another account reaches the
    // names inside it and opens whatever it can read there. Measured before the
    // fix: `facts` called a `0701` directory `ok` ("owner-only") and a `0601`
    // file `ok`, because only `0o006` and `0o060` were counted.
    let temp = TempDir::new("reach");
    let dir = temp.join("data");
    std::fs::create_dir_all(&dir).expect("dir");
    if !private::modes_apply() {
        // Windows: there is no mode to weaken, and the ACL is not inspected.
        assert!(private::facts(&dir).ok);
        return;
    }
    // The mode half is `#[cfg(unix)]`, not merely skipped at run time: the closure
    // below calls `PermissionsExt::from_mode`, which does not exist on Windows, so an
    // unguarded body made `cargo test -p atlas-platform` fail to *compile* there —
    // measured with `cargo check --tests --target x86_64-pc-windows-msvc`:
    // `error[E0433]: cannot find 'unix' in 'os'` and `E0599 … 'from_mode'`. The early
    // `return` above only covers what a Windows *run* would do; it cannot cover what
    // the compiler must still accept.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let chmod = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .expect("chmod")
        };

        chmod(&dir, 0o701);
        let facts = private::facts(&dir);
        assert!(
            !facts.ok && facts.world_reachable && facts.beyond_owner,
            "0701 on a directory is world traversal: {:?}",
            facts
        );
        chmod(&dir, 0o710);
        assert!(!private::facts(&dir).ok, "0710 is group traversal");
        chmod(&dir, 0o700);
        assert!(private::facts(&dir).ok, "0700 is what ATLAS asks for");

        let file = dir.join("atlas.db");
        std::fs::write(&file, b"x").expect("db");
        chmod(&file, 0o601);
        assert!(!private::facts(&file).ok, "0601 is not 0600");
        chmod(&file, 0o604);
        assert!(!private::facts(&file).ok, "0604 is world readable");
        chmod(&file, 0o640);
        assert!(!private::facts(&file).ok, "0640 is group readable");
        // The special bits are not counted: they give another account nothing on a
        // file it cannot open, and a 0700 directory with the sticky bit is still 0700.
        chmod(&file, 0o600);
        chmod(&file, 0o4600);
        assert!(private::facts(&file).ok, "setuid on a 0600 file is not reach");
    }
}

#[test]
fn the_browser_profile_lives_inside_the_data_directory_not_a_shared_temp_dir() {
    // `/tmp` and `C:\Windows\Temp` are world-writable by design, so a private
    // profile there is one that another account can name, watch, or pre-create.
    // The layout's answer is inside `data/`, which is 0700.
    let temp = TempDir::new("profile-home");
    let home = location::Home::at(&temp.path);
    assert_eq!(home.chrome_profile(), home.data_dir.join("chrome-profile"));
    assert!(
        home.chrome_profile().starts_with(&home.data_dir),
        "{} is not inside {}",
        home.chrome_profile().display(),
        home.data_dir.display()
    );
    // And `host::temp_dir` is only ever a *parent* for scratch: a caller that
    // needs a private directory there creates it with `private::create_dir`,
    // which is 0700 from the mkdir and refuses a name someone else took.
    let scratch = host::temp_dir().join(format!("atlas-platform-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    private::create_dir(&scratch).expect("scratch");
    if private::modes_apply() {
        assert_eq!(private::mode_of(&scratch), Some(private::DIR_MODE));
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_home_directory_that_is_a_symlink_still_contains_its_own_data() {
    // A checkout, or a home, reached through a link is ordinary on macOS and on
    // any machine with a second volume mounted at ~/data. Both sides are
    // canonicalised, so the answers must agree whichever spelling is used — and
    // a `..` that climbs out of the link must still be refused.
    let temp = TempDir::new("home-symlink");
    let real = temp.join("real-root");
    std::fs::create_dir_all(real.join("data")).expect("real root");
    std::fs::write(real.join("data").join("atlas.db"), b"x").expect("db");

    #[cfg(unix)]
    {
        let home = temp.join("home");
        link_to(&real, &home);
        let db_from_link = home.join("data").join("atlas.db");
        let db_from_real = real.join("data").join("atlas.db");
        assert!(root::inside_root(&home, &db_from_link));
        assert!(root::inside_root(&home, &db_from_real));
        assert!(root::inside_root(&real, &db_from_link));
        assert!(
            root::same_path(&db_from_link, &db_from_real),
            "one file, two spellings"
        );
        assert!(!root::inside_root(&real, &home.join("..").join("elsewhere")));
        assert!(!root::inside_root(&home, &real.join("..").join("elsewhere")));
        // The privacy call on a path under a linked home is answered about that
        // path, not about the link.
        assert!(private::make_dir_private(real.join("data")).ok());
    }
    // The same file, named by its real path, is still inside a root given by the
    // real path — so nothing above depended on the link.
    assert!(root::inside_root(&real, &real.join("data").join("atlas.db")));
}

#[test]
fn a_log_whose_name_is_a_link_is_refused_rather_than_written_through() {
    // `open_append` is how the app creates `data/app.log` (and llama/chrome), and
    // it set the mode at creation but not `O_NOFOLLOW`: a log name that was a
    // link appended every log line to a file of whoever made the link — prompt
    // text and article titles included. Same rule as `create_options`, from the
    // same place, so the two cannot drift.
    let temp = TempDir::new("append-link");
    let log = temp.join("app.log");
    {
        use std::io::Write;
        let mut file = private::open_append(&log).expect("a fresh log opens");
        file.write_all(b"line\n").expect("write");
    }
    if private::modes_apply() {
        assert_eq!(
            private::mode_of(&log).map(|mode| mode & 0o777),
            Some(private::FILE_MODE),
            "0600 from the open"
        );
    }
    // An existing regular log is reopened, not refused: this is not a create-only.
    assert!(private::open_append(&log).is_ok());
    #[cfg(unix)]
    {
        let victim = temp.join("victim.txt");
        std::fs::write(&victim, b"original").expect("victim");
        let link = temp.join("linked.log");
        link_to(&victim, &link);
        assert!(
            private::open_append(&link).is_err(),
            "a log name taken by a link must not be appended through"
        );
        assert_eq!(std::fs::read(&victim).expect("read"), b"original");
    }
}

/// A printing test, not a guess: `canonical_text` runs on every configured path
/// and on both sides of every containment question, so its cost is part of the
/// answer to "does a `..`-correct resolver make the server slow". The path that
/// exists has to stay one `canonicalize` (the kernel is the authority and the
/// common case): the component walk is for a path that is **not** there, which is
/// the only case where a component has no file system answer.
#[test]
fn resolving_a_path_that_exists_is_one_syscall_not_a_walk() {
    use std::time::Instant;

    let temp = TempDir::new("canonical-cost");
    let deep = temp.join("a").join("b").join("c").join("d").join("atlas.db");
    std::fs::create_dir_all(deep.parent().expect("parent")).expect("dirs");
    std::fs::write(&deep, b"x").expect("db");
    // A path that is not there yet **and climbs out of the root**: the walk is
    // what answers it, and the answer has to be "outside".
    let missing = temp
        .join("not")
        .join("there")
        .join("..")
        .join("..")
        .join("..")
        .join("outside")
        .join("atlas.db");

    let rounds = 2_000;
    // The primitive, for scale, and the function under test, measured **interleaved**:
    // both are wall-clock numbers on a machine other agents are building on, so timing
    // 2000 `canonicalize` calls in one window and 2000 `inside_root` calls in the next
    // compares two different moments. Measured: this test failed 2 of 6 full-suite runs
    // (`inside_root took … against a … canonicalize`) while passing 8 of 8 alone — the
    // neighbour's load, not the code. Alternating one call of each puts both under the
    // same load; the thresholds below are unchanged.
    let mut primitive_us = 0.0;
    let mut existing_us = 0.0;
    for _ in 0..rounds {
        let started = Instant::now();
        let _ = std::fs::canonicalize(&deep);
        primitive_us += started.elapsed().as_secs_f64() * 1e6;

        let started = Instant::now();
        assert!(root::inside_root(&temp.path, &deep));
        existing_us += started.elapsed().as_secs_f64() * 1e6;
    }
    let one = primitive_us / rounds as f64;
    let per = existing_us / rounds as f64;
    eprintln!("canonicalize(existing): {:.2} us/call", one);
    eprintln!(
        "inside_root(existing, 4 levels): {:.2} us/call over {} calls",
        per, rounds
    );
    // The rule this pins: two paths that exist cost **two** `canonicalize` calls
    // (one per side) plus the text comparison, not a walk over every component. A
    // walk would be `depth` syscalls per side; the assertion is a multiple of the
    // measured primitive so it does not encode this machine's speed.
    assert!(
        per < one * 8.0,
        "inside_root took {:.2} us/call against a {:.2} us canonicalize: the common case became the walk",
        per,
        one
    );

    let started = Instant::now();
    for _ in 0..rounds {
        assert!(!root::inside_root(&temp.path, &missing));
    }
    let per = started.elapsed().as_secs_f64() * 1e6 / rounds as f64;
    eprintln!("inside_root(missing tail, 3 `..`): {:.2} us/call", per);
    assert!(per < one * 40.0, "the walk took {:.2} us/call", per);
}
