//! File modes — `data/` is 0700, the database files are 0600.
//!
//! The *decisions* are `atlas-platform`'s: what the two modes are, that a mode is
//! set on POSIX and that the ACL inherited from the profile applies elsewhere, and
//! what counts as "readable beyond its owner". This module is the archive's view
//! of them — one entry per database file, in the shape `/api/status` reads — and
//! contains no second implementation of a platform rule.
//!
//! It exists so the archive is not readable by another account on the machine. It
//! is a mitigation, not a guarantee: SQLite creates `-wal` and `-shm` itself, which
//! is why [`crate::Store::tighten`] runs again after the pragmas.

use std::path::Path;

use atlas_platform::private;

/// `data/` itself.
pub const DATA_MODE: u32 = private::DIR_MODE;

/// Anything ATLAS writes inside `data/`: the database, `-wal`, `-shm`.
pub const FILE_MODE: u32 = private::FILE_MODE;

/// POSIX here, ACLs elsewhere.
pub const POSIX: bool = private::DIR_MODE == 0o700;

/// Mode of one file or directory, for `/api/status` and the ops check.
/// Mirrors `config.permissions()`: `mode` is `oct(mode)`, and `ok` means "not
/// readable beyond its owner".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermInfo {
    pub path: String,
    pub exists: bool,
    pub mode: String,
    pub world_readable: bool,
    pub group_readable: bool,
    pub ok: bool,
}

/// Modes of `data/` and of the database files inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionReport {
    pub posix: bool,
    pub entries: Vec<PermInfo>,
    pub exposed: Vec<String>,
    pub ok: bool,
}

/// One entry of [`PermissionReport`], from `atlas-platform`'s verdict.
pub fn permissions(path: &Path) -> PermInfo {
    let facts = private::facts(path);
    PermInfo {
        path: facts.path.clone(),
        exists: facts.exists,
        mode: facts.mode_text(),
        world_readable: facts.world_reachable,
        group_readable: facts.group_reachable,
        ok: facts.ok,
    }
}

/// Is a file mode a thing on this platform at all? The const above is a compile
/// time marker for the report; this is the question a caller should ask.
pub fn modes_apply() -> bool {
    private::modes_apply()
}

/// Create the directory, then make it owner-only. A directory that already exists
/// is fine; `true` means it is there.
pub fn ensure_dir(path: &Path) -> bool {
    let created = std::fs::create_dir_all(path).is_ok();
    private::make_dir_private(path);
    created
}

/// Force a file's mode. A missing file is not an error, and on Windows there is
/// nothing to force.
pub fn ensure_file(path: &Path, mode: u32) -> bool {
    let kind = if mode == private::DIR_MODE {
        private::Kind::Dir
    } else {
        private::Kind::File
    };
    matches!(
        private::make_private(path, kind),
        private::Privacy::Changed | private::Privacy::Already
    )
}

/// `OpenOptions` that create the database `0600` from the first byte.
pub fn create_options() -> std::fs::OpenOptions {
    private::create_options()
}
