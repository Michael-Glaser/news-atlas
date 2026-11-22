//! Which project root a command line names — one answer, made absolute.
//!
//! `atlas start` and `atlas doctor` are told the root in the same way (`--root
//! DIR`, or nothing), so the rule is written once here instead of twice at the
//! process boundary.
//!
//! Two decisions, and both matter:
//!
//! * **Nothing means discovery.** No `--root` is not "the current directory": the
//!   binary may be started from anywhere (`atlas` on `PATH`, a shortcut, another
//!   project's shell), and `atlas-platform` already answers where the project is
//!   from the running binary. Asking the working directory instead would make the
//!   app's own layout depend on where the user happened to type the command.
//! * **A named root is made absolute.** `--root .` used to be recorded verbatim,
//!   so the lock file said `root=.` and the second-instance refusal read "another
//!   ATLAS is already running … for ." — a sentence that cannot tell a user which
//!   directory to go and stop. Every line that names the root (the boot's `root`
//!   note, the lock record, the doctor's header) now names the same absolute
//!   directory.
//!
//! Nothing here invents a layout: the four directories under the root are
//! `atlas-config::Paths`, and finding the project is `atlas-platform::root`.

use std::path::{Path, PathBuf};

use atlas_config::Paths;

use crate::args::RootOption;

/// The [`Paths`] one command line works on.
pub fn paths(root: &RootOption) -> Paths {
    match root {
        Some(root) => Paths::at(absolute(root)),
        None => Paths::discover(),
    }
}

/// `path` as an absolute path, without touching the file system.
///
/// `std::path::absolute` collapses `.` and joins the working directory; it does
/// not resolve symlinks, which is what we want — a symlinked root stays the
/// directory the user named. A path it cannot make absolute (a working directory
/// that has been deleted, an unreadable drive) is returned unchanged, because a
/// refused boot for *that* reason would be a worse answer than a relative one.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_root_is_made_absolute_so_every_message_can_name_it() {
        assert_eq!(absolute(Path::new(".")), std::env::current_dir().unwrap());
        assert_eq!(absolute(Path::new("sub/dir")), {
            let mut expected = std::env::current_dir().unwrap();
            expected.push("sub");
            expected.push("dir");
            expected
        });
        // An absolute root is passed through, symlinks and all.
        let already = std::env::temp_dir().join("atlas-home-absolute");
        assert_eq!(absolute(&already), already);
    }

    #[test]
    fn a_relative_root_reaches_the_paths_and_the_lock_record_as_a_directory() {
        let relative = paths(&Some(PathBuf::from("relative-root")));
        assert!(relative.root.is_absolute(), "{:?}", relative.root);
        assert!(relative.root.ends_with("relative-root"));
        assert_eq!(relative.data_dir, relative.root.join("data"));
        // The `--root .` case: the record the next instance reads names the
        // directory, not a dot.
        let here = paths(&Some(PathBuf::from(".")));
        assert_eq!(here.root, std::env::current_dir().unwrap());
        assert!(!here.root.display().to_string().contains("/./"));
    }

    #[test]
    fn no_root_means_the_project_this_binary_belongs_to() {
        // Not the working directory: `atlas-platform` finds the project from the
        // running binary, and this test binary is built inside it.
        let paths = paths(&None);
        assert_eq!(paths.root, atlas_platform::root::app_root());
        assert!(
            paths.root.join("rust").join("Cargo.toml").is_file(),
            "the discovered root is not the workspace: {}",
            paths.root.display()
        );
        assert!(atlas_platform::root::looks_like_app_root(&paths.root));
    }
}
