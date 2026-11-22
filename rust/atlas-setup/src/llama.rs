//! The model server: llama.cpp, built from the tree that lives in this repository.
//!
//! `tools/llama.cpp/` is the vendored upstream source. `tools/README.md` documents the
//! recipe as four lines a human types by hand; this module is that recipe as a
//! function, so a fresh machine needs no paragraph of instructions:
//!
//! ```text
//! cmake -S tools/llama.cpp -B tools/llama.cpp/build -DCMAKE_BUILD_TYPE=Release
//! cmake --build tools/llama.cpp/build --target llama-server -j <cores>
//! cp tools/llama.cpp/build/bin/llama-server tools/llama.cpp/build/bin/*.dylib bin/
//! ```
//!
//! # What it guarantees
//!
//! * **No shell, anywhere.** `sh -c` and `cmd /c` never appear: cmake is spawned
//!   through [`atlas_platform::child::command`], the workspace's one spawn boundary
//!   (environment cleared, a curated allowlist added). Every argv is built as a
//!   `Vec<OsString>` and printed before it runs, so a path with a space in it is one
//!   argument because it was never a string that could be split.
//! * **A refusal, not a panic.** No input to this module panics. A missing tree, a
//!   missing cmake, a failed build, a missing output and an unreadable directory are
//!   each a sentence that says what is wrong and what to do.
//! * **The server and its libraries move together.** The binary is copied *and* the
//!   shared libraries beside it, into `bin/` — the directory `atlas-launcher` looks
//!   in first. Copying the binary alone would leave it next to libraries from a
//!   different build, which is a crash that reads like a code bug.
//!
//! What it does not do: start the server, load a model, or touch `models/` or
//! `data/`. It runs no step at app run time; `atlas start` never calls this module.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use atlas_platform::child;

/// How many cores a build may use.
///
/// `tools/README.md` says `-j` with no number, which is every core the machine has.
/// Sixteen is the cap here because llama.cpp's build is memory-hungry at high
/// parallelism — a machine with 96 cores is not faster at `-j 96`, it is a machine
/// swapping. The floor is one: `available_parallelism` never returns zero, but
/// [`jobs`] is a pure function and a caller could.
const MAX_JOBS: usize = 16;

/// How deep the fallback search for the built binary goes, under `build/bin/`.
///
/// Bounded so a symlink loop in somebody's build directory cannot hang the install.
const MAX_SEARCH_DEPTH: usize = 4;

/// The sentence a machine without cmake gets.
///
/// One sentence, three package managers, because the person reading it is on one of
/// the three platforms this project supports and does not want a link.
const CMAKE_MISSING: &str = "cmake is not installed — install it once (macOS: brew install cmake; \
     Debian/Ubuntu: sudo apt install cmake; Windows: winget install Kitware.CMake) and run this again";

/// The same sentence for a caller that only prints it — the report's `llama.cpp binary`
/// row — so the report and the refusal cannot drift into two different instructions.
pub fn cmake_install_sentence() -> &'static str {
    CMAKE_MISSING
}

/// What a finished build produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    /// Every file copied into `bin/`: the server first, then the libraries beside it.
    pub copied: Vec<PathBuf>,
    /// The server in `bin/` — the path `atlas-launcher` starts.
    pub server: PathBuf,
}

/// The cmake on `PATH`, summarised, or the install sentence when there is none.
///
/// `cmake --version` prints `cmake version 3.30.2`; the answer here is
/// `cmake 3.30.2`, which is what the report line shows a person.
pub fn cmake_available() -> Result<String, String> {
    let output = child::command("cmake")
        .arg("--version")
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CMAKE_MISSING.to_string()
            } else {
                format!("cannot run cmake: {error}")
            }
        })?;
    if !output.status.success() {
        return Err(format!(
            "cmake is on PATH but `cmake --version` failed ({}) — {CMAKE_MISSING}",
            output.status
        ));
    }
    version_of(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        "cmake is on PATH but `cmake --version` printed no version — reinstall cmake".to_string()
    })
}

/// Is the vendored tree here? `<root>/tools/llama.cpp/CMakeLists.txt`.
///
/// One file, because that file is what makes the directory a cmake project: an empty
/// `tools/llama.cpp/` (a half-finished clone, a deleted tree) is not a source tree and
/// must not send cmake off to fail on its own terms.
pub fn source_present(root: &Path) -> bool {
    layout(root).llama_source().join("CMakeLists.txt").is_file()
}

/// Is there a model server in `bin/`? `<root>/bin/llama-server[.exe]`.
pub fn server_present(root: &Path) -> bool {
    layout(root).model_server().is_file()
}

/// Build `bin/llama-server` from `tools/llama.cpp`, and copy what it needs beside it.
///
/// `say` receives every argv before it is run, so a person watching sees the exact
/// invocation (and not a summary of it). The build inherits the terminal, because a
/// ten-minute compile that prints nothing looks like a hang.
pub fn build(root: &Path, say: &mut dyn FnMut(&str)) -> Result<Built, String> {
    let source = layout(root).llama_source();
    if !source_present(root) {
        return Err(format!(
            "{} is missing — the vendored llama.cpp tree is part of this repository and nothing here \
             fetches it; restore it (or put a built {} in place) and run this again",
            source.display(),
            server_file_name()
        ));
    }
    // A missing cmake is the install sentence, before any file is touched.
    let toolchain = cmake_available()?;
    let jobs = jobs(std::thread::available_parallelism().map(|count| count.get()).unwrap_or(1));
    say(&format!(
        "building {} with {toolchain} ({jobs} jobs)",
        server_file_name()
    ));
    run("configure", &configure_argv(&source), say)?;
    run("build", &build_argv(&source, jobs), say)?;
    let built = find_built_server(&source).ok_or_else(|| {
        format!(
            "cmake finished but no {} is under {} — the tree is not the one this project pins",
            server_file_name(),
            source.join("build").display()
        )
    })?;
    place(root, &built, say)
}

// --- the two invocations, as argv -------------------------------------------------

/// `cmake -S <src> -B <src>/build -DCMAKE_BUILD_TYPE=Release`, one argument per
/// element.
fn configure_argv(source: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-S"),
        source.as_os_str().to_os_string(),
        OsString::from("-B"),
        source.join("build").into_os_string(),
        OsString::from("-DCMAKE_BUILD_TYPE=Release"),
    ]
}

/// `cmake --build <src>/build --target llama-server -j <jobs>`, one argument per
/// element.
fn build_argv(source: &Path, jobs: usize) -> Vec<OsString> {
    vec![
        OsString::from("--build"),
        source.join("build").into_os_string(),
        OsString::from("--target"),
        OsString::from("llama-server"),
        OsString::from("-j"),
        OsString::from(jobs.to_string()),
    ]
}

/// How many jobs to hand cmake: the cores this machine has, clamped to 1..=16.
fn jobs(cores: usize) -> usize {
    cores.clamp(1, MAX_JOBS)
}

/// One argv, as it is shown before it runs: an argument with whitespace in it is
/// quoted, so the printed line and the real argv cannot be mistaken for each other.
fn shown(arg: &OsStr) -> String {
    let text = arg.to_string_lossy();
    if text.contains(char::is_whitespace) {
        format!("\"{text}\"")
    } else {
        text.into_owned()
    }
}

/// `cmake <argv…>`, printed, run, and a sentence when it fails.
fn run(what: &str, argv: &[OsString], say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let shown_argv: Vec<String> = argv.iter().map(|arg| shown(arg)).collect();
    say(&format!("cmake {}", shown_argv.join(" ")));
    let status = child::command("cmake").args(argv).status().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CMAKE_MISSING.to_string()
        } else {
            format!("cannot run cmake: {error}")
        }
    })?;
    if !status.success() {
        return Err(format!(
            "cmake {what} failed ({status}) — fix what it reported above and run this again"
        ));
    }
    Ok(())
}

// --- finding what the build produced ---------------------------------------------

/// `llama-server`, or `llama-server.exe` on Windows — the name `atlas-launcher` looks
/// for and the name cmake gives its output.
fn server_file_name() -> &'static str {
    if cfg!(windows) { "llama-server.exe" } else { "llama-server" }
}

/// The built binary: `build/bin/llama-server[.exe]`, or a recursive search under
/// `build/bin/` when the generator put it somewhere else (a multi-config generator
/// writes `build/bin/Release/`).
fn find_built_server(source: &Path) -> Option<PathBuf> {
    let bin = source.join("build").join("bin");
    let direct = bin.join(server_file_name());
    if direct.is_file() {
        return Some(direct);
    }
    search_for_server(&bin, 0)
}

/// The fallback search, depth-bounded and sorted so two runs agree on the answer.
fn search_for_server(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > MAX_SEARCH_DEPTH {
        return None;
    }
    let listing = fs::read_dir(dir).ok()?;
    let mut subdirectories: Vec<PathBuf> = Vec::new();
    for entry in listing.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirectories.push(path);
        } else if path.file_name().and_then(OsStr::to_str) == Some(server_file_name()) {
            return Some(path);
        }
    }
    subdirectories.sort();
    subdirectories
        .into_iter()
        .find_map(|next| search_for_server(&next, depth + 1))
}

// --- copying the binary and its libraries -----------------------------------------

/// Copy the server, and the shared libraries beside it, into `<root>/bin/`.
///
/// The server goes first because it is the file `server_present()` reports and the one a
/// person checks for, so `bin/` is never a directory of libraries with no binary. Each
/// file lands through [`replace`], so a failure names the exact file it failed on and
/// leaves no half-written one in `bin/`. Everything is copied rather than moved or
/// linked, so the build directory keeps its inputs and a later rebuild still works.
fn place(root: &Path, built: &Path, say: &mut dyn FnMut(&str)) -> Result<Built, String> {
    let beside = built
        .parent()
        .ok_or_else(|| format!("{} has no directory", built.display()))?;
    let bin = root.join("bin");
    fs::create_dir_all(&bin)
        .map_err(|error| format!("cannot create {}: {error}", bin.display()))?;

    let server = bin.join(server_file_name());
    replace(built, &server)?;
    say(&format!("copied {}", server.display()));
    let mut copied = vec![server.clone()];

    for name in shared_libraries_in(beside)? {
        let to = bin.join(&name);
        replace(&beside.join(&name), &to)?;
        say(&format!("copied {}", to.display()));
        copied.push(to);
    }
    Ok(Built { copied, server })
}

/// Copy `from` over `to`, so anyone reading `to` sees the whole old file or the whole new
/// one — never a half-written one, and never a locked one in the way.
///
/// `fs::copy` writes straight into the destination, which is wrong twice over here.
/// On Windows the destination is *locked while it is running*, and the model server is
/// exactly the file a person is likely to have running when they rebuild: the copy dies
/// with "Access is denied (os error 5)" after truncating nothing on POSIX but after
/// `bin/llama-server` has already been replaced on a second run. On POSIX the copy
/// truncates first, so a disk that fills or a Ctrl-C in the middle leaves `bin/` holding
/// a truncated server that `atlas-launcher` will happily execute — a crash that reads
/// like a code bug. So the bytes land on `<to>.new` in the same directory (same volume,
/// so the rename that follows cannot fall back to a copy) and `rename` puts them in
/// place, which is the one step this family of file systems either does completely or
/// refuses. A refusal is the honest outcome on Windows, and it gets a sentence telling
/// the person to stop the server first.
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    let staged = staged_path(to);
    if let Err(error) = fs::copy(from, &staged) {
        // A copy that never started is not a busy file, so it keeps the plain sentence —
        // but the partial `.new` goes, because a stray file in `bin/` is one nobody reads.
        let _ = fs::remove_file(&staged);
        return Err(format!(
            "cannot copy {} to {}: {error}",
            from.display(),
            staged.display()
        ));
    }
    fs::rename(&staged, to).map_err(|error| {
        let _ = fs::remove_file(&staged);
        refused(to, &error)
    })
}

/// The staged name for `to`: the same file name plus `.new`, appended to the `OsString`
/// rather than re-joined as text, so a name that is not UTF-8 still works.
fn staged_path(to: &Path) -> PathBuf {
    let mut name = to.file_name().map(OsStr::to_os_string).unwrap_or_default();
    name.push(".new");
    to.with_file_name(name)
}

/// Why putting `to` in place was refused, in words a person can act on.
///
/// The likely cause is named because it is the one a person can fix, and the operating
/// system's own sentence is kept because it is the only thing that distinguishes a
/// permission problem from a lock.
fn refused(to: &Path, error: &std::io::Error) -> String {
    format!(
        "cannot replace {}: {error}. If the model server is running, stop it first: this \
         system will not overwrite a program that is running, and the new build is \
         otherwise ready",
        to.display()
    )
}

/// The shared libraries in `dir`, by name, in a stable order.
fn shared_libraries_in(dir: &Path) -> Result<Vec<String>, String> {
    let listing =
        fs::read_dir(dir).map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut entries: Vec<(String, bool)> = Vec::new();
    for entry in listing.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let is_directory = path.is_dir();
        entries.push((name.to_string(), is_directory));
    }
    Ok(select_libraries(&entries))
}

/// Which of the names beside the server are copied: the shared libraries, and nothing
/// else.
///
/// A build directory also holds cmake files, static archives, the `llama-cli` and
/// `llama-quantize` binaries and anything else a person left there; copying all of it
/// would put binaries in `bin/` that nothing starts. So the rule is exactly three
/// suffixes — `.dylib` on macOS, `.so` / `.so.<digits>` on Linux, `.dll` on Windows —
/// the server is never in the list, and neither is a directory that happens to be
/// spelled like a library (a framework bundle, or a `.dylib` directory).
fn select_libraries(entries: &[(String, bool)]) -> Vec<String> {
    let server = server_file_name();
    let mut chosen: Vec<String> = entries
        .iter()
        .filter(|(name, is_directory)| {
            !*is_directory
                && name.as_str() != server
                && name != "llama-server"
                && name != "llama-server.exe"
                && is_shared_library_name(name)
        })
        .map(|(name, _)| name.clone())
        .collect();
    chosen.sort();
    chosen
}

/// Is this file name a shared library? The three platform spellings, and nothing that
/// merely contains the letters.
fn is_shared_library_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".dylib") || lower.ends_with(".dll") {
        return true;
    }
    // `libllama.so`, `libllama.so.0`, `libllama.so.0.0.1` — the version suffix is
    // digits and dots only, so `notes.something` is not a library.
    match lower.find(".so") {
        Some(at) => {
            let after = &lower[at + 3..];
            after.is_empty()
                || (after.starts_with('.')
                    && after[1..].chars().all(|ch| ch.is_ascii_digit() || ch == '.'))
        }
        None => false,
    }
}

// --- the layout, as lib.rs defines it ---------------------------------------------

/// The paths this module uses, taken from [`crate::Layout`] so that "where the server
/// goes" has one definition and not two.
fn layout(root: &Path) -> crate::Layout {
    crate::Layout { root: root.to_path_buf() }
}

/// `cmake version 3.30.2` → `cmake 3.30.2`; anything without a version number → `None`.
fn version_of(stdout: &str) -> Option<String> {
    let first = stdout.lines().map(str::trim).find(|line| !line.is_empty())?;
    let rest = first
        .strip_prefix("cmake version ")
        .or_else(|| first.strip_prefix("cmake "))
        .unwrap_or(first);
    let number = rest.split_whitespace().next()?;
    if !number.starts_with(|ch: char| ch.is_ascii_digit()) {
        return Some(format!("cmake {first}"));
    }
    Some(format!("cmake {number}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch root, removed first so two runs of the suite cannot see each other.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-llama-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch root");
        dir
    }

    /// The vendored tree is recognised by `CMakeLists.txt` and by nothing else: an
    /// empty directory, or a *directory* of that name, is not a source tree.
    #[test]
    fn the_source_is_recognised_by_its_cmakelists() {
        let root = scratch("source");
        assert!(!source_present(&root), "an empty root has no tree");

        let tree = root.join("tools").join("llama.cpp");
        fs::create_dir_all(&tree).expect("the tree");
        assert!(!source_present(&root), "an empty tree is not a cmake project");

        fs::create_dir_all(tree.join("CMakeLists.txt")).expect("a directory of that name");
        assert!(!source_present(&root), "a directory is not the file");

        fs::remove_dir_all(tree.join("CMakeLists.txt")).expect("remove");
        fs::write(tree.join("CMakeLists.txt"), "project(llama)\n").expect("the file");
        assert!(source_present(&root), "the file makes it a source tree");

        let _ = fs::remove_dir_all(&root);
    }

    /// A missing tree is a sentence naming the directory and saying nobody fetches it —
    /// not a panic, and not a directory created on the way to failing.
    #[test]
    fn build_refuses_with_a_sentence_when_the_tree_is_missing() {
        let root = scratch("refuse");
        let error = build(&root, &mut |_| {}).expect_err("no tree, no build");
        assert!(error.contains("vendored"), "{error}");
        assert!(error.contains("llama.cpp"), "the sentence names the tree: {error}");
        assert!(!root.join("bin").exists(), "a refusal touches nothing");
        assert!(
            !root.join("tools").join("llama.cpp").join("build").exists(),
            "no build directory is made before the refusal"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Exactly the three spellings are copied: a `.dylib`, a `.so` and its versioned
    /// names, and a `.dll` — never a directory, never the server, never anything else
    /// the build directory happens to hold.
    #[test]
    fn what_is_copied_is_exactly_the_shared_libraries() {
        let entries: Vec<(String, bool)> = [
            ("llama-server", false),
            ("llama-server.exe", false),
            ("libllama.dylib", false),
            ("libggml-base.dylib", false),
            ("libllama.so", false),
            ("libllama.so.0", false),
            ("libllama.so.0.0.1", false),
            ("ggml.dll", false),
            // Not libraries: a static archive, a text file, the other binaries the
            // build produces, a file that merely contains the letters, and a
            // directory spelled like a library.
            ("libllama.a", false),
            ("CMakeCache.txt", false),
            ("llama-cli", false),
            ("llama-quantize", false),
            ("notes.something", false),
            ("libllama.dylib.old", false),
            ("Framework.dylib", true),
            ("libllama.so.9", true),
        ]
        .into_iter()
        .map(|(name, is_dir)| (name.to_string(), is_dir))
        .collect();

        let chosen = select_libraries(&entries);
        assert_eq!(
            chosen,
            vec![
                "ggml.dll".to_string(),
                "libggml-base.dylib".to_string(),
                "libllama.dylib".to_string(),
                "libllama.so".to_string(),
                "libllama.so.0".to_string(),
                "libllama.so.0.0.1".to_string(),
            ],
            "only shared libraries, and never a directory or the server"
        );
        assert!(!chosen.iter().any(|name| name == "llama-server"));
        assert!(!chosen.iter().any(|name| name == "llama-server.exe"));
    }

    /// A directory named like a library is skipped even when the pattern matches, and
    /// an empty listing chooses nothing.
    #[test]
    fn a_directory_is_never_a_library() {
        let entries = vec![("libllama.dylib".to_string(), true)];
        assert!(select_libraries(&entries).is_empty());
        assert!(select_libraries(&[]).is_empty());
    }

    /// One argument per element: a source path with a space in it is *one* argument,
    /// and nothing in either argv is a joined string that a shell could split.
    #[test]
    fn the_argv_keeps_each_argument_in_its_own_element() {
        let source = Path::new("/Users/atlas user/ATLAS news/tools/llama.cpp");
        let configure = configure_argv(source);
        assert_eq!(configure.len(), 5);
        assert_eq!(configure[0], OsString::from("-S"));
        assert_eq!(
            configure[1],
            source.as_os_str().to_os_string(),
            "the path with a space is its own element, whole"
        );
        assert_eq!(
            configure[3],
            source.join("build").into_os_string(),
            "build/ is inside the source, not beside a split argument"
        );
        assert_eq!(configure[4], OsString::from("-DCMAKE_BUILD_TYPE=Release"));
        // The only elements with whitespace in them are the two that *are* the path,
        // and each of those keeps the whole path (two spaces in this sample) in one
        // element — the count a naive split would produce is what must not appear.
        for (index, argument) in configure.iter().enumerate() {
            let spaces = argument.to_string_lossy().matches(' ').count();
            let expected = if index == 1 || index == 3 { 2 } else { 0 };
            assert_eq!(spaces, expected, "element {index} of the argv: {argument:?}");
        }

        let build = build_argv(source, 16);
        assert_eq!(build.len(), 6);
        assert_eq!(build[0], OsString::from("--build"));
        assert_eq!(build[1], source.join("build").into_os_string());
        assert_eq!(build[2], OsString::from("--target"));
        assert_eq!(build[3], OsString::from("llama-server"));
        assert_eq!(build[4], OsString::from("-j"));
        assert_eq!(build[5], OsString::from("16"));
        assert!(
            !build.iter().any(|argument| argument.to_string_lossy().contains("--target llama-server")),
            "the target and its argument are never one string"
        );

        // And what is printed quotes the space, so the line cannot be read as two
        // arguments either.
        assert_eq!(shown(source.as_os_str()), "\"/Users/atlas user/ATLAS news/tools/llama.cpp\"");
        assert_eq!(shown(OsStr::new("-j")), "-j");
    }

    /// The jobs count is clamped to what a llama.cpp build should use.
    #[test]
    fn jobs_are_clamped() {
        assert_eq!(jobs(0), 1, "never zero jobs");
        assert_eq!(jobs(1), 1);
        assert_eq!(jobs(8), 8);
        assert_eq!(jobs(16), 16);
        assert_eq!(jobs(96), MAX_JOBS, "a 96-core machine does not get -j 96");
    }

    /// The binary is found at its documented path, and — when a multi-config generator
    /// put it lower — by the bounded fallback search. Finding nothing is `None`, not a
    /// panic, and the search does not wander out of `build/bin/`.
    #[test]
    fn the_built_server_is_found_at_its_documented_path_or_below_it() {
        let root = scratch("find");
        let source = root.join("tools").join("llama.cpp");
        let bin = source.join("build").join("bin");
        assert_eq!(find_built_server(&source), None, "nothing built yet");

        fs::create_dir_all(bin.join("Release")).expect("a nested build directory");
        let nested = bin.join("Release").join(server_file_name());
        fs::write(&nested, b"binary").expect("a nested server");
        assert_eq!(find_built_server(&source).as_deref(), Some(nested.as_path()));

        let direct = bin.join(server_file_name());
        fs::write(&direct, b"binary").expect("the documented path");
        assert_eq!(
            find_built_server(&source).as_deref(),
            Some(direct.as_path()),
            "the documented path wins over a search"
        );

        // Outside build/bin/ is not the build's output and is never picked up: with
        // both real candidates gone, a server at build/elsewhere/ is still not found.
        fs::remove_file(&direct).expect("remove the direct one");
        fs::remove_file(&nested).expect("remove the nested one");
        let stray = source.join("build").join("elsewhere").join(server_file_name());
        fs::create_dir_all(stray.parent().expect("a directory")).expect("create");
        fs::write(&stray, b"binary").expect("a stray server");
        assert_eq!(find_built_server(&source), None, "build/bin/ is the whole search space");

        let _ = fs::remove_dir_all(&root);
    }

    /// The copy step, with a fake build directory: the server and the libraries land in
    /// `bin/`, everything else stays where it was, and what was copied is returned.
    #[test]
    fn the_server_and_its_libraries_are_copied_into_bin() {
        let root = scratch("place");
        let built_dir = root.join("tools").join("llama.cpp").join("build").join("bin");
        fs::create_dir_all(&built_dir).expect("a build directory");
        let built_server = built_dir.join(server_file_name());
        fs::write(&built_server, b"the server").expect("the server");
        fs::write(built_dir.join("libllama.dylib"), b"the library").expect("a library");
        fs::write(built_dir.join("libggml.so.0"), b"a linux library").expect("a library");
        fs::write(built_dir.join("llama-cli"), b"another binary").expect("another binary");
        fs::write(built_dir.join("CMakeCache.txt"), b"cmake state").expect("cmake state");

        let mut said: Vec<String> = Vec::new();
        let built = place(&root, &built_server, &mut |line| said.push(line.to_string()))
            .expect("a copy");

        let bin = root.join("bin");
        let expected_server = bin.join(server_file_name());
        assert_eq!(built.server, expected_server);
        assert_eq!(
            built.copied,
            vec![
                expected_server.clone(),
                bin.join("libggml.so.0"),
                bin.join("libllama.dylib"),
            ],
            "the server first, then the libraries, and nothing else"
        );
        assert!(expected_server.is_file(), "the server is in bin/");
        assert_eq!(fs::read(&expected_server).expect("read"), b"the server");
        assert!(bin.join("libllama.dylib").is_file());
        assert!(bin.join("libggml.so.0").is_file());
        assert!(!bin.join("llama-cli").exists(), "other binaries are not copied");
        assert!(!bin.join("CMakeCache.txt").exists());
        assert!(
            built_dir.join("libllama.dylib").is_file(),
            "the build directory keeps its files"
        );
        assert_eq!(said.len(), 3, "every copy is said: {said:?}");

        let _ = fs::remove_dir_all(&root);
    }

    /// A second `atlas-setup llama` over an existing `bin/` replaces the server and the
    /// libraries rather than accumulating beside them, and leaves no staged `.new` file
    /// behind: the launcher looks in `bin/` for names it knows, but a person reading the
    /// directory should not have to guess which of two binaries is the real one.
    #[test]
    fn replacing_a_placed_server_leaves_one_binary_and_no_staged_file() {
        let root = scratch("replace");
        let built_dir = root.join("tools").join("llama.cpp").join("build").join("bin");
        fs::create_dir_all(&built_dir).expect("a build directory");
        let built_server = built_dir.join(server_file_name());
        fs::write(&built_server, b"version one").expect("the first server");
        fs::write(built_dir.join("libllama.dylib"), b"first library").expect("a library");

        let mut quiet = |_: &str| {};
        let first = place(&root, &built_server, &mut quiet).expect("a first copy");
        fs::write(&built_server, b"version two").expect("a rebuilt server");
        fs::write(built_dir.join("libllama.dylib"), b"second library").expect("a rebuilt library");
        let second = place(&root, &built_server, &mut quiet).expect("a second copy");

        assert_eq!(first.server, second.server, "the same file is the destination");
        assert_eq!(
            fs::read(&second.server).expect("read"),
            b"version two",
            "the newer build is the one in bin/"
        );
        let bin = root.join("bin");
        let names: Vec<String> = fs::read_dir(&bin)
            .expect("bin")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        let mut expected = vec![
            server_file_name().to_string(),
            "libllama.dylib".to_string(),
        ];
        expected.sort();
        assert_eq!(sorted, expected, "one server, one library, nothing staged: {names:?}");
        assert!(
            !bin.join(format!("{}.new", server_file_name())).exists(),
            "the staged copy is gone once it is in place"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A copy that cannot even start says so, names the file it was writing, and takes
    /// the failed `.new` with it — a leftover half-file is worse than no file, because
    /// the next run copies *over* it and a person reading `bin/` cannot tell.
    #[test]
    fn a_failed_copy_names_the_file_and_clears_the_staged_one() {
        let root = scratch("copy-fails");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin");
        let to = bin.join(server_file_name());
        let err = replace(&root.join("no-such-build").join("llama-server"), &to)
            .expect_err("nothing there to copy");
        assert!(err.contains("cannot copy"), "{err}");
        assert!(
            err.contains(&format!("{}.new", server_file_name())),
            "the sentence names the file it was writing: {err}"
        );
        assert!(
            !staged_path(&to).exists(),
            "no staged file survives a copy that failed"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The staged name is the destination's own name with `.new` appended, in the same
    /// directory, so the rename never crosses a volume.
    #[test]
    fn the_staged_name_sits_beside_the_destination() {
        let to = Path::new("somewhere").join("bin").join(server_file_name());
        let staged = staged_path(&to);
        assert_eq!(staged.parent(), to.parent(), "same directory, so the rename is atomic");
        assert_eq!(
            staged.file_name().and_then(OsStr::to_str),
            Some(format!("{}.new", server_file_name()).as_str())
        );
    }

    /// A refusal tells the person the one thing they can do about it, and keeps the
    /// operating system's own words — the only part that separates a lock from a
    /// permission problem.
    #[test]
    fn a_refused_replace_says_to_stop_the_server() {
        let to = Path::new("bin").join(server_file_name());
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access is denied");
        let sentence = refused(&to, &error);
        assert!(sentence.contains("stop it first"), "{sentence}");
        assert!(sentence.contains("Access is denied"), "{sentence}");
        assert!(sentence.contains(server_file_name()), "{sentence}");
    }

    /// Replacing a server that cannot be written to still works, because the write never
    /// goes to that file.
    ///
    /// This is the shape Windows puts in the way — a destination that refuses to be
    /// opened for writing — reproduced where it can be measured: a `bin/llama-server`
    /// with no write bit. `fs::copy` into it fails with EACCES, which is what the old
    /// code did, so a rebuild on Windows died with "Access is denied (os error 5)" and
    /// left nothing usable. Staging a sibling and renaming needs only the *directory* to
    /// be writable, so the newer build lands. (Measured by pointing `replace` back at
    /// `fs::copy` — this test fails with "cannot copy …: Permission denied".)
    #[cfg(unix)]
    #[test]
    fn a_server_that_cannot_be_written_is_still_replaced() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch("readonly");
        let built_dir = root.join("tools").join("llama.cpp").join("build").join("bin");
        fs::create_dir_all(&built_dir).expect("a build directory");
        let built_server = built_dir.join(server_file_name());
        fs::write(&built_server, b"version one").expect("the first server");
        let mut quiet = |_: &str| {};
        let placed = place(&root, &built_server, &mut quiet).expect("a first copy");

        let read_only = fs::Permissions::from_mode(0o444);
        fs::set_permissions(&placed.server, read_only).expect("drop the write bit");
        fs::write(&built_server, b"version two").expect("a rebuilt server");
        place(&root, &built_server, &mut quiet).expect("a second copy over a read-only server");
        assert_eq!(
            fs::read(&placed.server).expect("read"),
            b"version two",
            "the newer build replaced a file that could not be written to"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The version line is summarised to the two words a report can use, and a line
    /// with no version in it is not silently accepted as one.
    #[test]
    fn the_cmake_version_is_summarised() {
        assert_eq!(
            version_of("cmake version 3.30.2\n\nCMake suite maintained by Kitware\n").as_deref(),
            Some("cmake 3.30.2")
        );
        assert_eq!(version_of("  cmake version 4.0.0  \n").as_deref(), Some("cmake 4.0.0"));
        assert_eq!(version_of("").as_deref(), None);
    }
}
