//! The app root: how it is found, and whether a path is inside it.
//!
//! A configured path stays inside the project — a hand-edited
//! `data/config.json` must not redirect a model load, a geo file or a Rust
//! binary at another file on the machine. Two questions, and only these:
//!
//! * [`app_root`] — where the project is, found from the running binary. The
//!   binary lives in `rust/target/release`, so the root is the nearest ancestor
//!   that looks like the project.
//! * [`inside_root`] — is this path under that root, **after** resolving
//!   symlinks and, on Windows, **ignoring case**. A path inside the project
//!   whose symlink target lives outside it is not inside.
//!
//! The lexical half of the answer is [`crate::winpath::inside`], which
//! normalises `.` and `..` in both separator styles; this module adds the file
//! system's opinion without ever re-implementing the text rules.
//!
//! ## The one thing that must not be got wrong
//!
//! [`canonical_text`] answers for a path that does not exist yet (a model that
//! has not been downloaded, a `data/` that is about to be created), and the
//! answer must be the path's **meaning**, not a repaired version of it. The
//! earlier version popped the missing components off the end, canonicalised what
//! was left, and pasted the missing ones back — which silently **deleted every
//! `..`** in the missing part. Measured before the fix:
//!
//! ```text
//! app/not-yet/../../outside/secret.txt   ->  app/not-yet/outside/secret.txt
//! inside_root(app, that path)            ->  true
//! app/data/inner/../../../elsewhere      ->  app/data/inner/elsewhere
//! inside_root(app, that path)            ->  true
//! ```
//!
//! The second one is `ATLAS_DATA_DIR`, and `std::fs::create_dir_all` on the same
//! text really creates `<temp>/elsewhere` — outside the root the check said the
//! data was inside. So `..` is now applied to the component before it, exactly
//! as the kernel applies it, and a `..` that walks out of the root makes the
//! answer "outside".

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::host;
use crate::winpath;

/// How many ancestors [`app_root`] will climb before it gives up.
pub const MAX_HOPS: usize = 8;

/// Does `dir` look like the ATLAS project root?
///
/// The markers are layout, not content — and each of them is a *pair* or a build
/// file, never a single common directory name. A binary started from anywhere
/// else falls back to the working directory, which is what the launcher sets.
///
/// A lone `data/` or a lone `server/` used to be enough, and both names are
/// common in other people's projects: a copied binary started inside one of them
/// wrote its archive into *that* project's `data/`. So:
///
/// * the workspace manifest (`rust/Cargo.toml`) — a checkout;
/// * `data/` together with one other directory the layout always has
///   (`public/`, `models/` or `bin/`) — an installed copy;
/// * `server/` together with `public/` — the legacy layout this project shipped
///   before it was Rust, still recognised so an upgrade finds its own `data/`.
pub fn looks_like_app_root(dir: &Path) -> bool {
    let has = |name: &str| dir.join(name).is_dir();
    dir.join("rust").join("Cargo.toml").is_file()
        || (has("data") && (has("public") || has("models") || has("bin")))
        || (has("server") && has("public"))
}

/// The first ancestor of one of `starts` that satisfies `looks_like`.
///
/// A parameter-driven walk, so a test can hand in its own directories and its
/// own predicate without a file system full of fake projects.
pub fn discover(
    starts: &[PathBuf],
    looks_like: &dyn Fn(&Path) -> bool,
    max_hops: usize,
) -> Option<PathBuf> {
    for start in starts {
        let mut current = Some(start.clone());
        let mut hops = 0;
        while let Some(dir) = current {
            if looks_like(&dir) {
                return Some(dir);
            }
            if hops >= max_hops {
                break;
            }
            hops += 1;
            current = dir.parent().map(|parent| parent.to_path_buf());
        }
    }
    None
}

/// The project root for this process.
///
/// `current_exe` first, then the working directory; a binary that is not inside
/// a project (a copy in `C:\Tools`, a test binary) lands on the working
/// directory, and a process with neither lands on `.`.
pub fn app_root() -> PathBuf {
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starts.push(dir.to_path_buf());
        }
        // The executable *file* used to be pushed here as a second start. It can
        // never look like a root, so it only spent the hop budget asking the same
        // questions about the same parent twice.
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Some(found) = discover(&starts, &looks_like_app_root, MAX_HOPS) {
        return found;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// A path resolved as far as the file system can answer: the part that exists is
/// canonicalised (symlinks resolved), and every component after it is applied to
/// that result — including `..`, which pops the component before it.
///
/// The two halves are kept apart because they mean different things. `real` is a
/// canonical path, so it has no symlink and no `..` in it, and popping its last
/// component keeps it canonical. `tail` holds components that do not exist, so
/// they cannot be symlinks; a `..` after one of them pops *that* component, which
/// is what the kernel does for a path that exists (`a/b/c/..` is `a/b` whether or
/// not `c` does). Pushing a component and finding that it now resolves means the
/// file system had an opinion after all (a symlink, a case difference, a
/// `/var` → `/private/var`), and the answer moves to it.
fn resolve_components(path: &Path) -> (PathBuf, Vec<OsString>) {
    // No working-directory seed: a *resolvable* relative path becomes absolute
    // inside `canonicalize` (it answers the absolute form), so seeding only
    // decided what happens to a relative path that is **not** there — and there
    // the answer has to be its own text, not the text with a directory prepended.
    // That matters on this side of a boundary: `atlas-config` hands in Windows
    // path text on POSIX, and `C:\app` must answer `C:/app`, not
    // `<cwd>/C:/app`.
    let mut real = PathBuf::new();
    let mut tail: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                real = PathBuf::from(prefix.as_os_str());
                tail.clear();
            }
            Component::RootDir => {
                real.push(Component::RootDir.as_os_str());
                tail.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => match tail.last() {
                // `..` cancels the component before it, named or missing. Two
                // `..` in a row do not cancel each other: `../..` is two levels
                // up, not ".".
                Some(last) if last != OsStr::new("..") => {
                    tail.pop();
                }
                _ => {
                    if real.as_os_str().is_empty() {
                        // A relative path that climbed past its own start: the
                        // `..` has nothing to cancel and is kept, so the text
                        // still means what it says.
                        tail.push(OsString::from(".."));
                    } else {
                        // At a root the pop fails and the path stays there, as
                        // the kernel does for `/..` and `C:\..`.
                        let _ = real.pop();
                    }
                }
            },
            Component::Normal(name) => {
                tail.push(name.to_os_string());
                let mut candidate = real.clone();
                for part in &tail {
                    candidate.push(part);
                }
                if let Ok(canonical) = std::fs::canonicalize(&candidate) {
                    real = canonical;
                    tail.clear();
                }
            }
        }
    }
    (real, tail)
}

/// A path as comparable text: every symlink resolved as far as it exists,
/// separators unified, the extended-length prefix removed.
///
/// The deepest existing ancestor is canonicalised and the components after it are
/// applied on top (`..` included, see [`resolve_components`]), so a model file
/// that does not exist yet still gets an answer — and the answer is where that
/// path would be, not a repaired spelling of it. `std::fs::canonicalize` on
/// Windows answers `\\?\C:\…`; that prefix is dropped here, because the caller
/// compares against a path that never had one.
///
/// A path that **does** exist goes through one `canonicalize` and nothing else, so
/// the common case (a static asset, the archive) costs one syscall. The walk is
/// the fallback for a path that is not there yet, which is the only case where a
/// component can have no file system answer.
pub fn canonical_text(path: &Path) -> String {
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => {
            let (mut real, tail) = resolve_components(path);
            for part in tail {
                real.push(part);
            }
            real
        }
    };
    let text = winpath::strip_verbatim(&winpath::unify(&resolved.to_string_lossy()));
    winpath::normalise(&text)
}

/// Is `target` inside `root`? Canonicalised, case-insensitive on Windows.
///
/// A `..` in either separator style is collapsed *before* the comparison, so
/// `app/../secrets` and `app\..\..\Windows` are both refused; a symlink is
/// resolved first, so `app/link-to-etc` is refused even though its text starts
/// with `app/`.
pub fn inside_root(root: &Path, target: &Path) -> bool {
    let base = canonical_text(root);
    let candidate = canonical_text(target);
    winpath::inside(&base, &candidate, host::os())
}

/// [`inside_root`] over text, with the canonicalisation done by the caller and
/// the platform named.
///
/// This is the function a test uses, because it needs no files: the lexical
/// rules (both separator styles, `..`, case, drive letters) are the same rule
/// the canonicalising caller applies.
pub fn inside_text(root: impl AsRef<Path>, target: &str, os: host::Os) -> bool {
    let base = canonical_text(root.as_ref());
    winpath::inside(&base, target, os)
}

/// Do two paths name the same file? Symlinks resolved, case folded where the
/// file system folds it, `\\?\` ignored.
///
/// This is the comparison a static-file server needs: `canonicalize` answers
/// `\\?\C:\a\b` and the path it was given was `C:\a\b`, so a plain `==` reports
/// "a symlink" for every file on Windows.
pub fn same_path(left: &Path, right: &Path) -> bool {
    winpath::same_path(
        &canonical_text(left),
        &canonical_text(right),
        host::os(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_walks_up_to_the_marker_and_not_beyond() {
        let starts = vec![PathBuf::from("/x/app/rust/target/release")];
        let found = discover(&starts, &|dir: &Path| {
            dir == Path::new("/x/app") || dir == Path::new("/x")
        }, MAX_HOPS);
        assert_eq!(found, Some(PathBuf::from("/x/app")));
    }

    #[test]
    fn discovery_gives_up_after_the_hop_budget() {
        let starts = vec![PathBuf::from("/a/b/c/d/e/f/g/h/i/j/k")];
        assert_eq!(discover(&starts, &|_dir: &Path| false, 3), None);
    }

    #[test]
    fn a_directory_that_belongs_to_something_else_is_not_the_app_root() {
        // `data/` and `server/` are ordinary directory names in other people's
        // projects. Either one alone used to be enough to be called ATLAS, so a
        // copied binary started inside another project would use *that*
        // project's `data/` for its archive.
        let temp = std::env::temp_dir().join(format!("atlas-platform-markers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let make = |name: &str, dirs: &[&str]| {
            let path = temp.join(name);
            std::fs::create_dir_all(&path).expect("root");
            for dir in dirs {
                std::fs::create_dir_all(path.join(dir)).expect("dir");
            }
            path
        };

        let stranger = make("other-project", &["data"]);
        assert!(!looks_like_app_root(&stranger));
        let server_only = make("server-project", &["server"]);
        assert!(!looks_like_app_root(&server_only));

        // An installed copy: the data directory plus one of the layout's own.
        let installed = make("installed", &["data", "public"]);
        assert!(looks_like_app_root(&installed));
        let distribution = make("distribution", &["data", "bin"]);
        assert!(looks_like_app_root(&distribution));
        // The legacy layout, recognised so an upgrade finds its own `data/`.
        let legacy = make("legacy", &["server", "public"]);
        assert!(looks_like_app_root(&legacy));
        // And a checkout.
        let checkout = make("checkout", &["rust"]);
        std::fs::write(checkout.join("rust").join("Cargo.toml"), b"[workspace]\n").expect("manifest");
        assert!(looks_like_app_root(&checkout));

        let _ = std::fs::remove_dir_all(&temp);
    }
}
