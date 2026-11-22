//! File modes: `data/` is 0700, the files inside it are 0600.
//!
//! The *decisions* are `atlas-platform`'s ([`atlas_platform::private`]): the two
//! modes, that a mode is set on POSIX, that the ACL inherited from the profile
//! applies on Windows, and what counts as "not readable beyond its owner". This
//! module is `config.py`'s published shape of those decisions, so `server/` and
//! `/api/status` keep the keys they always read.
//!
//! It does not: find the files (the layout does) or enforce anything the kernel
//! does not — it reports what the filesystem says.

use std::path::{Path, PathBuf};

use atlas_platform::private;

use crate::paths::{mode_text, path_is_file, real_path, show, Paths};
use crate::spawn::Platform;
use crate::warn::warn;

/// `data/` itself.
pub const DATA_MODE: u32 = private::DIR_MODE;

/// Anything ATLAS writes inside `data/` (db, wal, shm, logs, config).
pub const FILE_MODE: u32 = private::FILE_MODE;

/// True on a POSIX machine, where these modes mean something.
pub fn posix() -> bool {
    private::modes_apply()
}

/// What protects a path on `os`: a mode here, an inherited ACL on Windows.
///
/// Both halves take the platform instead of a `cfg!`, so the Windows sentence is
/// testable on the machine that builds this crate — the same shape
/// `atlas_platform::private::modes_apply_on` has. `/api/security` publishes the
/// answer as `permissions.basis`, and a person reading `"basis": "posix-mode"`
/// knows the mode beside it was read rather than assumed.
pub fn permission_basis(os: Platform) -> &'static str {
    match os {
        Platform::Posix => "posix-mode",
        Platform::Windows => "windows-acl-inherited",
    }
}

/// Did ATLAS read the thing that protects a path?
///
/// A mode: yes. A Windows ACL: no — no `icacls`, no win32 call in a
/// `forbid(unsafe_code)` crate, so the report says `acl_inspected: false` rather
/// than presenting `ok: true` as if the directory had been checked. That `ok` is
/// "nothing was found exposed", not "a private ACL was seen".
pub fn acl_inspected(os: Platform) -> bool {
    match os {
        Platform::Posix => true,
        Platform::Windows => false,
    }
}

/// Create `dir` (if needed) and make it owner-only. A no-op mode on Windows,
/// where ACLs decide and there is nothing to report here.
///
/// The `mkdir` carries [`DATA_MODE`] where the platform can do that
/// ([`atlas_platform::private::create_dir`]): `create_dir_all` answers `0755`
/// under the usual `umask`, which leaves a window in which another account reads
/// the archive that is about to be written into the directory. A directory that
/// already exists is still forced to the mode, which is what this function is
/// for; a missing parent (a fresh `ATLAS_DATA_DIR`) falls back to creating the
/// levels, each one forced afterwards.
pub fn ensure_dir(dir: impl AsRef<Path>) -> PathBuf {
    let path = dir.as_ref().to_path_buf();
    match private::create_dir(&path) {
        // Made owner-only by the `mkdir` itself.
        Ok(()) => return path,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            note_dir_privacy(&path);
            return path;
        }
        Err(_) => {}
    }
    if std::fs::create_dir_all(&path).is_err() {
        return path;
    }
    note_dir_privacy(&path);
    path
}

/// Say so when a directory could not be made owner-only (or is a link).
///
/// The directory is used either way — refusing to start because `data/` is
/// `0755` would be a worse trade than starting and saying it — but a `chmod`
/// that the kernel refused is the one outcome a caller cannot see: a `data/`
/// created by another account, or on a read-only mount, worked until now without
/// a word, and `/api/security`'s entries were the only place it showed.
fn note_dir_privacy(path: &Path) {
    let verdict = private::make_dir_private(path);
    if !verdict.ok() {
        warn(&format!(
            "{} {}: the mode ATLAS needs is {} and the file system did not take it",
            show(path),
            verdict.note(),
            mode_text(DATA_MODE)
        ));
    }
}

/// Force `path` to [`FILE_MODE`]. A missing file is not an error.
///
/// A path that is itself a link is refused, not followed — the *decision* is
/// [`atlas_platform::private`]'s (`Privacy::Linked`: `chmod` applies to the
/// target, so a `data/app.log` that is a link pointed at somebody else's file).
/// What lives here is saying so: a hardening step that silently did nothing is
/// the one thing this crate refuses to be.
pub fn ensure_file(path: impl AsRef<Path>) -> bool {
    if !posix() {
        return false;
    }
    let verdict = private::make_file_private(path.as_ref());
    if verdict == private::Privacy::Linked {
        warn(&format!(
            "refusing to set a file mode on {}: it is a link, and the mode ATLAS sets is the one on the file it wrote",
            show(path.as_ref())
        ));
    }
    matches!(verdict, private::Privacy::Changed | private::Privacy::Already)
}

/// True when `path` itself is a link — the platform's answer
/// ([`atlas_platform::private::is_link`]), re-published under the name this
/// crate's callers read.
pub fn is_symlink(path: impl AsRef<Path>) -> bool {
    private::is_link(path)
}

/// One file's mode, and whether another account can read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermInfo {
    pub path: String,
    pub exists: bool,
    pub mode: String,
    pub world_readable: bool,
    pub group_readable: bool,
    pub ok: bool,
    /// Where a symlink points, when the path is one. `None` for a real file.
    ///
    /// A report that showed only `path`/`mode` was answering about a path it had
    /// not read: `stat` on `<root>/data` follows the link, so a link to a
    /// directory elsewhere reported *that* directory's mode under this project's
    /// name and said nothing about the redirection.
    pub link: Option<String>,
}

impl PermInfo {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value};
        let mut map = Map::new();
        map.insert("path".to_string(), Value::String(self.path.clone()));
        map.insert("exists".to_string(), Value::Bool(self.exists));
        map.insert("mode".to_string(), Value::String(self.mode.clone()));
        map.insert("world_readable".to_string(), Value::Bool(self.world_readable));
        map.insert("group_readable".to_string(), Value::Bool(self.group_readable));
        map.insert("ok".to_string(), Value::Bool(self.ok));
        map.insert(
            "link".to_string(),
            match &self.link {
                Some(target) => Value::String(target.clone()),
                None => Value::Null,
            },
        );
        Value::Object(map)
    }
}

/// Mode of one file or directory, plus whether it is readable beyond its owner.
pub fn permissions(path: &str) -> PermInfo {
    if path.is_empty() {
        return PermInfo {
            path: String::new(),
            exists: false,
            mode: String::new(),
            world_readable: false,
            group_readable: false,
            ok: false,
            link: None,
        };
    }
    let facts = private::facts(path);
    PermInfo {
        path: facts.path.clone(),
        exists: facts.exists,
        mode: facts.mode_text(),
        world_readable: facts.world_reachable,
        group_readable: facts.group_reachable,
        ok: facts.ok,
        link: if is_symlink(path) {
            Some(real_path(path))
        } else {
            None
        },
    }
}

/// Permissions of `data/` and the files ATLAS writes there, and whether every
/// one of them is owner-only.
///
/// `ok` is false when *nothing was read*: an empty list of existing paths is not
/// a clean bill of health, it is a report about nothing, and `/api/security`
/// published it as `ok: true` while every entry said `exists: false`.
pub fn permission_report(paths: &[String]) -> (bool, Vec<PermInfo>) {
    let entries: Vec<PermInfo> = paths.iter().map(|path| permissions(path)).collect();
    let seen: Vec<&PermInfo> = entries.iter().filter(|entry| entry.exists).collect();
    let ok = !seen.is_empty() && seen.iter().all(|entry| entry.ok);
    (ok, entries)
}

/// chmod `data/` to 0700 and the files ATLAS owns to 0600. Returns the changed
/// paths.
pub fn harden_data_files(paths: &Paths, extra: &[String]) -> Vec<String> {
    ensure_dir(&paths.data_dir);
    let mut candidates: Vec<String> = vec![
        show(&paths.config_file),
        show(&paths.db),
        show(&paths.db_wal),
        show(&paths.db_shm),
        show(&paths.log_app),
        show(&paths.log_llama),
        show(&paths.log_chrome),
    ];
    candidates.extend(extra.iter().filter(|path| !path.is_empty()).cloned());
    let mut changed = Vec::new();
    for path in candidates {
        if !path.is_empty() && path_is_file(&path) && ensure_file(&path) {
            changed.push(path);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::Platform;
    use crate::tests::TempDir;

    #[test]
    fn a_report_about_nothing_is_not_a_clean_bill_of_health() {
        // `permission_report` used to answer `ok: true` for a list of paths that
        // did not exist: the `all()` ran over an empty filter, so `/api/security`
        // published `"ok": true` beside eight entries saying `"exists": false`.
        // "Nothing was exposed" and "nothing was read" are different sentences.
        let dir = TempDir::new("modes-empty");
        let paths = dir.at();
        let missing: Vec<String> = vec![show(&paths.data_dir), show(&paths.db)];
        let (ok, entries) = permission_report(&missing);
        assert!(!ok, "a report about files that do not exist is not ok");
        assert!(entries.iter().all(|entry| !entry.exists));

        // Once they exist and are owner-only, the answer is yes — and it is yes
        // because a mode was read, not because the list was empty.
        ensure_dir(&paths.data_dir);
        std::fs::write(&paths.db, "x").expect("db");
        ensure_file(&paths.db);
        let (ok, entries) = permission_report(&missing);
        assert!(ok, "{:?}", entries);
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.path == show(&paths.data_dir))
                .map(|entry| entry.mode.clone()),
            Some(DATA_MODE_TEXT.to_string())
        );
    }

    #[cfg(unix)]
    const DATA_MODE_TEXT: &str = "0o700";
    #[cfg(not(unix))]
    const DATA_MODE_TEXT: &str = "windows-acl";

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_never_the_file_that_gets_a_mode() {
        use std::os::unix::fs::PermissionsExt;
        // `set_permissions` applies to the target, so an `app.log` that is a link
        // to another account's file had ATLAS tightening a file it never wrote.
        // The mode ATLAS sets is the mode of the file ATLAS made.
        let dir = TempDir::new("modes-link");
        let outside = TempDir::new("modes-link-outside");
        let victim = outside.path.join("victim.txt");
        std::fs::write(&victim, "not ours").expect("victim");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let paths = dir.at();
        ensure_dir(&paths.data_dir);
        std::os::unix::fs::symlink(&victim, &paths.log_app).expect("symlink");

        assert!(is_symlink(&paths.log_app));
        assert!(!ensure_file(&paths.log_app), "a symlink must not be chmod'ed");
        let after = std::fs::metadata(&victim).expect("meta").permissions().mode() & 0o777;
        assert_eq!(after, 0o644, "the link's target was chmod'ed: {:o}", after);

        // And the report says what it is: a link, and where it goes.
        let entry = permissions(&show(&paths.log_app));
        assert!(entry.exists);
        assert_eq!(
            entry.link.as_deref().map(|link| link.trim_end_matches('/')),
            Some(real_path(&show(&victim)).trim_end_matches('/')),
            "{:?}",
            entry.link
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_data_directory_that_could_not_be_made_private_is_said_out_loud() {
        // `ensure_dir` used to chmod and drop the verdict: a `data/` owned by
        // another account, on a read-only mount, or a link, was used anyway and
        // nothing was reported. Starting is still the right trade; being silent
        // about it is not.
        let dir = TempDir::new("ensure-dir-link");
        let outside = TempDir::new("ensure-dir-outside");
        let paths = dir.at();
        std::os::unix::fs::symlink(&outside.path, &paths.data_dir).expect("symlink");
        ensure_dir(&paths.data_dir);
        assert!(
            crate::warnings()
                .iter()
                .any(|note| note.contains("data") && note.contains("not private")),
            "{:?}",
            crate::warnings()
        );
    }

    #[test]
    fn the_windows_half_says_the_acl_was_not_inspected() {
        // Windows has no mode, and this crate reads no ACL: no `icacls`, no win32
        // call. Publishing `ok: true` there was a claim about a directory nobody
        // looked at, so the report carries the basis and whether it was inspected.
        // Both are asked of the platform, so the Windows answer is testable on the
        // machine that builds this crate.
        assert_eq!(permission_basis(Platform::Posix), "posix-mode");
        assert_eq!(permission_basis(Platform::Windows), "windows-acl-inherited");
        assert!(acl_inspected(Platform::Posix));
        assert!(!acl_inspected(Platform::Windows));
        assert_eq!(posix(), permission_basis(crate::platform()) == "posix-mode");
    }
}
