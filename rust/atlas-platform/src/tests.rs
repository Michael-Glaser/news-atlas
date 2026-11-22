//! The platform suite: Windows-shaped input, on whichever machine runs the tests.
//!
//! Every rule that only a Windows CI could exercise used to sit behind
//! `#[cfg(target_os = "windows")]`, which is how the browser lookup stayed wrong
//! for years without a failing test. The rules are pure functions of (text, `Os`)
//! now, so this file is the Windows half of the suite and it runs everywhere.
//!
//! What a POSIX machine still cannot prove natively, and what covers it instead:
//!
//! * `std::fs::canonicalize` returning `\\?\C:\…` — the *comparison* is tested
//!   against verbatim text ([`root::same_path`] via `winpath`), the syscall is
//!   not.
//! * an ACL, a drive letter and a case-insensitive file system — the decisions are
//!   tested, the kernel is not.
//! * `chmod` being refused, and `CREATE_NO_WINDOW` — one branch of each is
//!   compiled out here; both are reported as untested in the crate report.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{host, location, private, root, winpath};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A directory under the real temp dir, removed on drop. Created with
/// `create_dir_all` so the same code works when `TMPDIR` is a deep path.
///
/// `pub(crate)` because narrative tests outgrew this file: the path and
/// permission suite in `tests_paths` uses the same helper rather than a second
/// one that could drift.
pub(crate) struct TempDir {
    pub(crate) path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(label: &str) -> TempDir {
        let serial = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = host::temp_dir().join(format!(
            "atlas-platform-{}-{}-{}",
            std::process::id(),
            label,
            serial
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir { path }
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// separators, drive letters, UNC, mixed separators
// ---------------------------------------------------------------------------

#[test]
fn a_backslash_path_and_a_slash_path_are_one_path() {
    assert!(winpath::same_path(
        r"C:\Program Files\ATLAS\data\atlas.db",
        "C:/Program Files/ATLAS/data/atlas.db",
        host::Os::Windows
    ));
    // Mixed separators inside one string are the same path too.
    assert!(winpath::same_path(
        r"C:\a\b/c\d",
        "C:/a/b/c/d",
        host::Os::Windows
    ));
    assert_eq!(
        winpath::normalise(r"C:\a\b/c\d"),
        "C:/a/b/c/d",
        "a mixed path normalises to one spelling"
    );
}

#[test]
fn case_is_folded_on_windows_and_not_on_posix() {
    assert!(winpath::same_path(r"C:\App\DATA", r"c:\app\data", host::Os::Windows));
    assert!(!winpath::same_path("/App/DATA", "/app/data", host::Os::Posix));
}

#[test]
fn a_drive_letter_is_absolute_and_a_drive_relative_path_is_not() {
    assert_eq!(winpath::shape(r"C:\a"), winpath::Shape::DriveAbsolute);
    assert!(winpath::shape(r"C:\a").is_absolute());
    // `C:a` is relative to the current directory *on* C: it is not absolute, and
    // it is the case that silently resolves somewhere else.
    assert_eq!(winpath::shape("C:a"), winpath::Shape::DriveRelative);
    assert!(!winpath::shape("C:a").is_absolute());
    assert_eq!(winpath::normalise("C:a/../b"), "C:b");
}

#[test]
fn a_unc_share_keeps_its_prefix_through_normalisation() {
    assert_eq!(winpath::shape(r"\\server\share\a"), winpath::Shape::Unc);
    assert_eq!(
        winpath::normalise(r"\\server\share\a\..\b"),
        "//server/share/b"
    );
    // The share is a root: `..` cannot climb above it.
    assert_eq!(
        winpath::normalise(r"\\server\share\..\..\..\x"),
        "//server/share/x"
    );
    // And it is never mistaken for a local path that starts with a slash.
    assert!(!winpath::same_path(r"\\server\share\a", "/server/share/a", host::Os::Windows));
}

#[test]
fn the_extended_length_prefix_is_dropped_before_comparison() {
    // What `canonicalize` answers on Windows, against what the caller had.
    assert!(winpath::same_path(
        r"\\?\C:\app\public\index.html",
        r"C:\app\public\index.html",
        host::Os::Windows
    ));
    assert_eq!(
        winpath::strip_verbatim(r"\\?\C:\app"),
        "C:/app",
        "the DOS-era prefix is not part of the path"
    );
    assert_eq!(
        winpath::strip_verbatim(r"\\?\UNC\server\share\a"),
        "//server/share/a"
    );
}

// ---------------------------------------------------------------------------
// names Windows would silently reinterpret
// ---------------------------------------------------------------------------

#[test]
fn a_reserved_device_name_is_refused_whatever_its_extension() {
    for name in ["CON", "con", "NUL", "nul.gguf", "COM1", "lpt9.txt", "AUX", "PRN", "CONOUT$"] {
        assert!(
            matches!(
                winpath::name_problem(name),
                Some(winpath::Problem::ReservedDevice(_))
            ),
            "{} must be refused",
            name
        );
    }
    // A component that merely starts with those letters is fine.
    for name in ["console.log", "nullable.rs", "conditional", "COM10", "LPT0"] {
        assert_eq!(winpath::name_problem(name), None, "{} must be allowed", name);
    }
    // And the whole path is judged component by component.
    assert!(matches!(
        winpath::path_problem(r"C:\app\models\NUL.gguf", host::Os::Windows),
        Some(winpath::Problem::ReservedDevice(_))
    ));
    // On POSIX the same name is a perfectly good file name.
    assert_eq!(winpath::path_problem("/app/models/NUL", host::Os::Posix), None);
}

#[test]
fn a_trailing_dot_or_space_is_refused_because_windows_drops_it() {
    assert_eq!(
        winpath::name_problem("models."),
        Some(winpath::Problem::TrailingDot)
    );
    assert_eq!(
        winpath::name_problem("models "),
        Some(winpath::Problem::TrailingSpace)
    );
    assert!(winpath::path_problem(r"C:\app\data.", host::Os::Windows).is_some());
    // A dot *inside* the name is ordinary.
    assert_eq!(winpath::name_problem("atlas.db"), None);
    // And a trailing dot is a legal POSIX file name.
    assert_eq!(winpath::path_problem("/app/data.", host::Os::Posix), None);
}

#[test]
fn an_alternate_data_stream_is_not_a_file_name() {
    assert_eq!(
        winpath::name_problem("notes.txt:hidden"),
        Some(winpath::Problem::Illegal(':'))
    );
    // … but the drive colon is a prefix, not a name.
    assert_eq!(winpath::path_problem(r"C:\app\notes.txt", host::Os::Windows), None);
}

#[test]
fn the_characters_a_name_cannot_hold_are_refused() {
    for bad in ['<', '>', ':', '"', '|', '?', '*'] {
        assert_eq!(
            winpath::name_problem(&format!("a{}b", bad)),
            Some(winpath::Problem::Illegal(bad))
        );
    }
    assert_eq!(winpath::name_problem("a\u{1}b"), Some(winpath::Problem::Control));
    assert_eq!(winpath::path_problem("", host::Os::Windows), Some(winpath::Problem::Empty));
}

#[test]
fn the_device_namespace_is_not_a_file_path() {
    assert_eq!(
        winpath::path_problem(r"\\.\pipe\atlas", host::Os::Windows),
        Some(winpath::Problem::DeviceNamespace)
    );
}

// ---------------------------------------------------------------------------
// inside the root: canonicalised, and dot-dot in both separator styles
// ---------------------------------------------------------------------------

#[test]
fn dot_dot_is_rejected_in_both_separator_styles_after_canonicalisation() {
    let temp = TempDir::new("downdot");
    let app = temp.join("app");
    let outside = temp.join("outside");
    std::fs::create_dir_all(app.join("sub")).expect("app");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("secret.txt"), b"x").expect("write");

    // The path is inside the root textually…
    let climbing = app.join("..").join("outside").join("secret.txt");
    assert!(climbing.to_string_lossy().contains("app"));
    // … and is refused anyway, because `..` is canonicalised away first.
    assert!(
        !root::inside_root(&app, &climbing),
        "{} escaped {}",
        climbing.display(),
        app.display()
    );
    // The plain form of the same file, one directory up, is refused too.
    assert!(!root::inside_root(&app, &outside.join("secret.txt")));

    // A real child is inside, and so is the root itself.
    assert!(root::inside_root(&app, &app.join("sub")));
    assert!(root::inside_root(&app, &app));
}

#[test]
fn dot_dot_is_rejected_on_a_windows_shaped_root_in_both_styles() {
    // Pure text: the rule that a canonicalising caller applies, with no files.
    for target in [
        r"C:\app\..\..\Windows\System32\config\SAM",
        "C:/app/../../Windows/System32/config/SAM",
        r"C:\app\..\app2\secret",
        r"C:/app\..\..\etc/passwd",
    ] {
        assert!(
            !root::inside_text(r"C:\app", target, host::Os::Windows),
            "{} must be refused",
            target
        );
    }
    // The legitimate cases still pass, case-insensitively.
    for target in [r"C:\app\sub\file.txt", r"C:\App\SUB\file.txt", "C:/app/sub"] {
        assert!(
            root::inside_text(r"C:\app", target, host::Os::Windows),
            "{} must be allowed",
            target
        );
    }
    // A sibling whose name merely starts with the root's name is not inside.
    assert!(!root::inside_text(
        r"C:\app",
        r"C:\application\x",
        host::Os::Windows
    ));
    // A UNC root is closed at its share.
    assert!(!root::inside_text(
        r"\\srv\share\app",
        r"\\srv\share\app\..\..\other",
        host::Os::Windows
    ));
}

#[test]
fn a_posix_root_rejects_dot_dot_and_a_backslash_that_climbs() {
    assert!(!root::inside_text("/app", "/app/../etc/passwd", host::Os::Posix));
    assert!(!root::inside_text("/app", r"/app\..\..\etc\passwd", host::Os::Posix));
    assert!(root::inside_text("/app", "/app/sub/file", host::Os::Posix));
    // POSIX does not fold case, so this is a different directory.
    assert!(!root::inside_text("/app", "/APP/sub", host::Os::Posix));
}

#[test]
fn a_symlink_out_of_the_root_is_refused_where_the_platform_allows_one() {
    let temp = TempDir::new("symlink");
    let app = temp.join("app");
    let outside = temp.join("outside");
    std::fs::create_dir_all(app.join("data")).expect("app");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("model.gguf"), b"x").expect("write");

    #[cfg(unix)]
    {
        let link = app.join("data").join("model.gguf");
        std::os::unix::fs::symlink(outside.join("model.gguf"), &link).expect("symlink");
        assert!(
            !root::inside_root(&app, &link),
            "a symlink to a file outside the root must not be inside it"
        );
    }
    // Everywhere else the target is simply outside the root.
    assert!(!root::inside_root(&app, &outside.join("model.gguf")));
}

#[test]
fn canonicalisation_of_a_path_that_does_not_exist_yet_still_answers() {
    let temp = TempDir::new("missing");
    let app = temp.join("app");
    std::fs::create_dir_all(&app).expect("app");
    let future = app.join("models").join("not-downloaded.gguf");
    let text = root::canonical_text(&future);
    assert!(text.ends_with("app/models/not-downloaded.gguf"), "{}", text);
    assert!(root::inside_root(&app, &future));
    assert!(!text.contains(r"\\?"), "the verbatim prefix must be gone: {}", text);
}

// ---------------------------------------------------------------------------
// the layout and the privacy verdict
// ---------------------------------------------------------------------------

#[test]
fn the_model_is_found_in_the_models_directory_newest_name_last() {
    let temp = TempDir::new("models");
    let models = temp.join("models");
    std::fs::create_dir_all(&models).expect("models");
    for name in ["b.gguf", "a.gguf", "notes.txt"] {
        std::fs::write(models.join(name), b"x").expect("write");
    }
    let found = location::gguf_in(&models);
    let names: Vec<String> = found
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["a.gguf", "b.gguf"]);
    assert_eq!(
        found.last().map(|path| path.file_name().unwrap().to_string_lossy().to_string()),
        Some("b.gguf".to_string())
    );
    // A directory with no models answers with an empty list, never a panic.
    assert!(location::gguf_in(temp.join("absent")).is_empty());
}

#[test]
fn a_private_file_really_is_owner_only_on_posix() {
    let temp = TempDir::new("private");
    let file = temp.join("atlas.db");
    std::fs::write(&file, b"x").expect("write");
    let verdict = private::make_file_private(&file);
    if private::modes_apply() {
        assert_eq!(verdict, private::Privacy::Changed);
        assert_eq!(private::mode_of(&file), Some(private::FILE_MODE));
        // A second call has nothing left to do and says so.
        assert_eq!(
            private::make_file_private(&file),
            private::Privacy::Already
        );
        let facts = private::facts(&file);
        assert!(facts.exists && facts.ok && !facts.beyond_owner);
        assert_eq!(facts.mode_text(), "0o600");
    } else {
        assert_eq!(verdict, private::Privacy::Inherited);
        assert!(!private::modes_apply());
    }
}

#[test]
fn a_private_directory_is_0700_on_posix_and_never_claims_a_chmod_elsewhere() {
    let temp = TempDir::new("private-dir");
    let dir = temp.join("data");
    std::fs::create_dir_all(&dir).expect("dir");
    let verdict = private::make_dir_private(&dir);
    if private::modes_apply() {
        assert_eq!(private::mode_of(&dir), Some(private::DIR_MODE));
        assert_eq!(verdict, private::Privacy::Changed);
    } else {
        assert_eq!(verdict, private::Privacy::Inherited);
        assert_eq!(verdict.note(), "Windows: the ACL inherited from the user profile applies");
    }
}

// ---------------------------------------------------------------------------
// the platform answers themselves
// ---------------------------------------------------------------------------

#[test]
fn the_executable_suffix_follows_the_platform() {
    assert_eq!(host::exe_suffix_of(host::Os::Windows), ".exe");
    assert_eq!(host::exe_suffix_of(host::Os::Posix), "");
    assert_eq!(host::exe_suffix(), host::exe_suffix_of(host::os()));
    let binary = host::app_binary(Path::new("/b"), "atlas-guard");
    assert!(binary.ends_with(format!("atlas-guard{}", host::exe_suffix())));
}

#[test]
fn the_user_agent_token_is_the_real_platform() {
    let token = host::navigator_platform();
    if cfg!(windows) {
        assert!(token.starts_with("Windows NT"));
    } else if cfg!(target_os = "macos") {
        assert!(token.starts_with("Macintosh"));
    } else {
        assert!(token.starts_with("X11"));
    }
}

#[test]
fn a_platform_name_is_never_silently_posix() {
    assert_eq!(host::from_name("Windows"), Some(host::Os::Windows));
    assert_eq!(host::from_name("darwin"), Some(host::Os::Posix));
    assert_eq!(host::from_name("plan9"), None);
}

#[test]
fn the_path_list_separator_is_the_platform_one() {
    let separator = host::path_list_separator();
    assert_eq!(separator, if cfg!(windows) { ';' } else { ':' });
}

// ---------------------------------------------------------------------------
// a project directory with spaces in it, and where the root comes from
// ---------------------------------------------------------------------------

#[test]
fn a_home_directory_with_spaces_is_one_directory_end_to_end() {
    // `/Users/Ann Marie/…` is an ordinary macOS home. Nothing may split it, quote
    // it, or treat the space as a separator — the layout, the canonicalised
    // comparison, the privacy call and the model lookup all have to agree.
    let temp = TempDir::new("home with spaces");
    let project = temp.join("news atlas");
    std::fs::create_dir_all(project.join("models")).expect("models");
    std::fs::create_dir_all(project.join("data")).expect("data");
    std::fs::write(project.join("models").join("m.gguf"), b"x").expect("model");

    let home = location::Home::at(&project);
    assert_eq!(home.data_dir, project.join("data"));
    assert_eq!(home.public_dir(), project.join("public"));
    assert!(home.db().to_string_lossy().contains("news atlas/data/atlas.db"));

    // Canonicalisation keeps the space, and containment answers for the real
    // directory: the project is inside itself, its sibling is not (a prefix is
    // not containment).
    let text = root::canonical_text(&home.db());
    assert!(text.contains("news atlas/data/atlas.db"), "{}", text);
    assert!(root::inside_root(&project, &home.db()));
    assert!(!root::inside_root(&project, &temp.join("news atlas2")));

    // The Windows spelling of the same question: one component, not two.
    assert!(root::inside_text(
        r"C:\Users\Ann Marie\news atlas",
        r"C:\Users\Ann Marie\news atlas\data\atlas.db",
        host::Os::Windows
    ));
    assert!(!root::inside_text(
        r"C:\Users\Ann Marie\news atlas",
        r"C:\Users\Ann Marie\news atlas2\data\atlas.db",
        host::Os::Windows
    ));

    // The model lookup and the mode of the directory work with a space in it.
    assert_eq!(location::gguf_in(&home.models_dir).len(), 1);
    assert!(private::make_dir_private(&home.data_dir).ok());
    if private::modes_apply() {
        assert_eq!(private::mode_of(&home.data_dir), Some(private::DIR_MODE));
    }
}

#[test]
fn the_app_root_is_derived_from_the_binary_and_never_written_in() {
    // The property the launcher depends on: the root is found by walking up from
    // this executable, so a moved or renamed checkout still resolves. A path
    // compiled in with `env!` would answer the *builder's* directory here, and
    // the first two assertions are what catch that.
    let exe = std::env::current_exe().expect("this test binary");
    let found = root::app_root();
    assert!(
        exe.starts_with(&found),
        "{} is not under the discovered root {}",
        exe.display(),
        found.display()
    );
    for component in found.components() {
        let name = component.as_os_str().to_string_lossy().to_string();
        assert!(
            name != "target" && name != "deps" && name != "build",
            "the root {} is a build directory",
            found.display()
        );
    }
    // The walk lands on a directory the crate recognises, and it is stable.
    assert!(root::looks_like_app_root(&found), "{}", found.display());
    assert_eq!(root::app_root(), found);
    // The budget has to cover cargo's own layout — the test binary sits at
    // `target/<profile>/build/<crate>/<hash>/out/<binary>`, six levels below
    // `target` — or the walk silently gives up and the *working directory*
    // becomes the root, which for a shortcut is not the project at all.
    let depth = exe
        .strip_prefix(&found)
        .map(|rest| rest.components().count())
        .unwrap_or(0);
    assert!(
        depth <= root::MAX_HOPS + 1,
        "this binary is {} levels under {}, and MAX_HOPS is {}",
        depth,
        found.display(),
        root::MAX_HOPS
    );
    // And every path in the layout is under it, so nothing can escape the
    // project by being written somewhere else.
    let home = location::Home::discover();
    assert!(home.data_dir.starts_with(&found), "{}", home.data_dir.display());
    assert!(home.models_dir.starts_with(&found), "{}", home.models_dir.display());
    assert!(home.bin_dir.starts_with(&found), "{}", home.bin_dir.display());
}

// ---------------------------------------------------------------------------
// what the rules cost
// ---------------------------------------------------------------------------

/// A printing test, not a guess: the pure path rules run on every configured
/// path (boot, every spawn, every static-file request), so their cost is part of
/// the design rather than a footnote. The ceiling is deliberately loose — this
/// catches a rewrite that turns a slice comparison into a regex, not a slow CI.
#[test]
fn the_windows_path_rules_are_slice_work_not_a_regex() {
    use std::time::Instant;

    let rounds = 20_000;
    let base = r"C:\Program Files\ATLAS";
    let target = r"C:\Program Files\ATLAS\data\..\data\atlas.db";
    let started = Instant::now();
    let mut yes = 0usize;
    for _ in 0..rounds {
        if winpath::inside(base, target, host::Os::Windows) {
            yes += 1;
        }
    }
    let per = started.elapsed().as_secs_f64() * 1e9 / rounds as f64;
    eprintln!(
        "inside(Win): {:.0} ns/call over {} calls ({} inside)",
        per, rounds, yes
    );
    assert_eq!(yes, rounds);
    assert!(per < 50_000.0, "inside() took {:.0} ns/call", per);

    let started = Instant::now();
    let mut total = 0usize;
    for _ in 0..rounds {
        total += winpath::normalise(target).len();
    }
    let per = started.elapsed().as_secs_f64() * 1e9 / rounds as f64;
    eprintln!("normalise(Win): {:.0} ns/call, {} bytes", per, total / rounds);
    assert!(per < 50_000.0, "normalise() took {:.0} ns/call", per);

    // The name rules: one call per segment of every configured path.
    let started = Instant::now();
    for _ in 0..rounds {
        assert!(winpath::name_problem("atlas.db").is_none());
    }
    let per = started.elapsed().as_secs_f64() * 1e9 / rounds as f64;
    eprintln!("name_problem: {:.0} ns/call", per);
    assert!(per < 50_000.0, "name_problem() took {:.0} ns/call", per);
}
